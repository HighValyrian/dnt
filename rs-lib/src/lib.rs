// Copyright 2018-2021 the Deno authors. All rights reserved. MIT license.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
#[macro_use]
extern crate lazy_static;

use graph::ModuleGraphOptions;
use loader::MappedSpecifierEntry;
use mappings::Mappings;
use specifiers::Specifiers;
use text_changes::apply_text_changes;
use visitors::get_deno_comment_directive_text_changes;
use visitors::get_deno_global_text_changes;
use visitors::get_module_specifier_text_changes;
use visitors::GetDenoGlobalTextChangesParams;
use visitors::GetModuleSpecifierTextChangesParams;

pub use deno_ast::ModuleSpecifier;
pub use loader::LoadResponse;
pub use loader::Loader;
pub use utils::url_to_file_path;

use crate::declaration_file_resolution::TypesDependency;

mod declaration_file_resolution;
mod graph;
mod loader;
mod mappings;
mod parser;
mod specifiers;
mod text_changes;
mod utils;
mod visitors;

#[cfg_attr(feature = "serialization", derive(serde::Serialize))]
#[cfg_attr(feature = "serialization", serde(rename_all = "camelCase"))]
#[derive(Debug, PartialEq)]
pub struct OutputFile {
  pub file_path: PathBuf,
  pub file_text: String,
}

#[cfg_attr(feature = "serialization", derive(serde::Serialize))]
#[cfg_attr(feature = "serialization", serde(rename_all = "camelCase"))]
#[derive(Debug, PartialEq)]
pub struct Dependency {
  pub name: String,
  pub version: String,
}

#[cfg_attr(feature = "serialization", derive(serde::Serialize))]
#[cfg_attr(feature = "serialization", serde(rename_all = "camelCase"))]
#[derive(Debug, PartialEq)]
pub struct TransformOutput {
  pub main: TransformOutputEnvironment,
  pub test: TransformOutputEnvironment,
  pub warnings: Vec<String>,
}

#[cfg_attr(feature = "serialization", derive(serde::Serialize))]
#[cfg_attr(feature = "serialization", serde(rename_all = "camelCase"))]
#[derive(Debug, PartialEq, Default)]
pub struct TransformOutputEnvironment {
  pub entry_points: Vec<String>,
  pub files: Vec<OutputFile>,
  pub shim_used: bool,
  pub dependencies: Vec<Dependency>,
}

pub struct TransformOptions {
  pub entry_points: Vec<ModuleSpecifier>,
  pub test_entry_points: Vec<ModuleSpecifier>,
  pub shim_package_name: String,
  pub loader: Option<Box<dyn Loader>>,
  pub specifier_mappings: Option<HashMap<ModuleSpecifier, String>>,
}

pub async fn transform(options: TransformOptions) -> Result<TransformOutput> {
  if options.entry_points.is_empty() {
    anyhow::bail!("at least one entry point must be specified");
  }

  let shim_package_name = options.shim_package_name;
  let ignored_specifiers = options
    .specifier_mappings
    .as_ref()
    .map(|t| t.keys().map(ToOwned::to_owned).collect());

  let (module_graph, specifiers) =
    crate::graph::ModuleGraph::build_with_specifiers(ModuleGraphOptions {
      entry_points: options.entry_points.clone(),
      test_entry_points: options.test_entry_points.clone(),
      ignored_specifiers: ignored_specifiers.as_ref(),
      loader: options.loader,
    })
    .await?;

  let mappings = Mappings::new(&module_graph, &specifiers)?;
  let mut specifier_mappings = options.specifier_mappings.unwrap_or_default();
  for (key, entry) in specifiers.main.mapped.iter().chain(specifiers.test.mapped.iter()) {
    specifier_mappings
      .insert(key.clone(), entry.to_specifier.clone());
  }

  // todo: parallelize
  let warnings = get_declaration_warnings(&specifiers);
  let mut main_environment = TransformOutputEnvironment {
    entry_points: options
      .entry_points
      .iter()
      .map(|p| mappings.get_file_path(p).to_string_lossy().to_string())
      .collect(),
    dependencies: get_dependencies(specifiers.main.mapped),
    ..Default::default()
  };
  let mut test_environment = TransformOutputEnvironment {
    entry_points: options
      .test_entry_points
      .iter()
      .map(|p| mappings.get_file_path(p).to_string_lossy().to_string())
      .collect(),
    dependencies: get_dependencies(specifiers.test.mapped),
    ..Default::default()
  };
  for specifier in specifiers
    .local
    .iter()
    .chain(specifiers.remote.iter())
    .chain(specifiers.types.iter().map(|(_, d)| &d.selected.specifier))
  {
    let module = module_graph.get(specifier);
    let environment = if specifiers.test_modules.contains(specifier) {
      &mut test_environment
    } else {
      &mut main_environment
    };
    let parsed_source = module.parsed_source.clone();

    let text_changes = parsed_source.with_view(|program| {
      let mut text_changes =
        get_deno_global_text_changes(&GetDenoGlobalTextChangesParams {
          program: &program,
          top_level_context: parsed_source.top_level_context(),
          shim_package_name: shim_package_name.as_str(),
        });
      if !text_changes.is_empty() {
        environment.shim_used = true;
      }
      text_changes.extend(get_deno_comment_directive_text_changes(&program));
      text_changes.extend(get_module_specifier_text_changes(
        &GetModuleSpecifierTextChangesParams {
          specifier,
          module_graph: &module_graph,
          mappings: &mappings,
          program: &program,
          specifier_mappings: &specifier_mappings,
        },
      ));

      text_changes
    });

    let file_path = mappings.get_file_path(specifier).to_owned();
    environment.files.push(OutputFile {
      file_path,
      file_text: apply_text_changes(
        parsed_source.source().text().to_string(),
        text_changes,
      ),
    });
  }

  Ok(TransformOutput {
    main: main_environment,
    test: test_environment,
    warnings,
  })
}

fn get_dependencies(mappings: BTreeMap<ModuleSpecifier, MappedSpecifierEntry>) -> Vec<Dependency> {
  let mut dependencies = mappings
    .into_iter()
    .filter_map(|entry| {
      if let Some(version) = entry.1.version {
        Some(Dependency {
          name: entry.1.to_specifier,
          version,
        })
      } else {
        None
      }
    })
    .collect::<Vec<_>>();
  dependencies.sort_by(|a, b| a.name.cmp(&b.name));
  dependencies
}

fn get_declaration_warnings(specifiers: &Specifiers) -> Vec<String> {
  let mut messages = Vec::new();
  for (code_specifier, d) in specifiers.types.iter() {
    if d.selected.referrer.scheme() == "file" {
      let local_referrers =
        d.ignored.iter().filter(|d| d.referrer.scheme() == "file");
      for dep in local_referrers {
        messages.push(get_dep_warning(
          code_specifier,
          dep,
          &d.selected,
          "Supress this warning by having only one local file specify the declaration file for this module.",
        ));
      }
    } else {
      for dep in d.ignored.iter() {
        messages.push(get_dep_warning(
          code_specifier,
          dep,
          &d.selected,
          "Supress this warning by specifying a declaration file for this module locally via `@deno-types`.",
        ));
      }
    }
  }
  return messages;

  fn get_dep_warning(
    code_specifier: &ModuleSpecifier,
    dep: &TypesDependency,
    selected_dep: &TypesDependency,
    post_message: &str,
  ) -> String {
    format!("Duplicate declaration file found for {}\n  Specified {} in {}\n  Selected {}\n  {}", code_specifier, dep.specifier, dep.referrer, selected_dep.specifier, post_message)
  }
}