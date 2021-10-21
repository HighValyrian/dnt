// Copyright 2018-2021 the Deno authors. All rights reserved. MIT license.

use std::collections::BTreeMap;
use std::collections::HashSet;

use anyhow::Result;
use deno_ast::ModuleSpecifier;
use deno_graph::Module;

use crate::graph::ModuleGraph;

pub struct DeclarationFileResolution {
  pub selected: TypesDependency,
  /// Specified declaration dependencies that were ignored.
  pub ignored: Vec<TypesDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypesDependency {
  /// The module being specified.
  pub specifier: ModuleSpecifier,
  /// The module that specified the specifier.
  pub referrer: ModuleSpecifier,
}

pub fn resolve_declaration_file_mappings(
  module_graph: &ModuleGraph,
  modules: &[&Module],
) -> Result<BTreeMap<ModuleSpecifier, DeclarationFileResolution>> {
  let mut type_dependencies = BTreeMap::new();

  for module in modules {
    fill_types_for_module(module, &mut type_dependencies)?;
  }

  // get the resolved type dependencies
  let mut mappings = BTreeMap::new();
  for (code_specifier, deps) in type_dependencies.into_iter() {
    let deps = deps.into_iter().collect::<Vec<_>>();
    let selected_dep =
      select_best_types_dep(module_graph, &code_specifier, &deps);

    // get the declaration file specifiers that weren't used
    let mut ignored = deps
      .into_iter()
      .filter(|d| d.specifier != selected_dep.specifier)
      .collect::<Vec<_>>();
    ignored.sort();

    mappings.insert(
      code_specifier,
      DeclarationFileResolution {
        selected: selected_dep,
        ignored,
      },
    );
  }

  Ok(mappings)
}

/// This resolution process works as follows:
///
/// 1. Prefer using a declaration file specified in local code over remote. This allows the user
///    to override what is potentially done remotely and in the worst case provide their own declaration file.
/// 2. Next prefer when the referrer is from the declaration file itself (ex. x-deno-types header)
/// 3. Finally use the declaration file that is the largest.
fn select_best_types_dep(
  module_graph: &ModuleGraph,
  code_specifier: &ModuleSpecifier,
  deps: &[TypesDependency],
) -> TypesDependency {
  assert!(!deps.is_empty());
  let mut selected_dep = &deps[0];
  for dep in &deps[1..] {
    let is_dep_referrer_local = dep.referrer.scheme() == "file";
    let is_dep_referrer_code = &dep.referrer == code_specifier;

    let is_selected_referrer_local = selected_dep.referrer.scheme() == "file";
    let is_selected_referrer_code = &selected_dep.referrer == code_specifier;

    let should_replace = if is_dep_referrer_local && !is_selected_referrer_local
    {
      true
    } else if is_dep_referrer_local == is_selected_referrer_local {
      if is_selected_referrer_code {
        false
      } else if is_dep_referrer_code {
        true
      } else {
        // as a last resort, use the declaration file that's the largest
        let dep_file_len = module_graph.get(&dep.specifier).source.len();
        let selected_dep_file_len =
          module_graph.get(&selected_dep.specifier).source.len();
        dep_file_len > selected_dep_file_len
      }
    } else {
      false
    };
    if should_replace {
      selected_dep = dep;
    }
  }
  selected_dep.clone()
}

fn fill_types_for_module(
  module: &Module,
  type_dependencies: &mut BTreeMap<ModuleSpecifier, HashSet<TypesDependency>>,
) -> Result<()> {
  // check for the module specifying its type dependency
  match &module.maybe_types_dependency {
    Some((text, Some(Err(err)))) => anyhow::bail!(
      "Error resolving types for {} with reference {}. {}",
      module.specifier,
      text,
      err.to_string()
    ),
    Some((_, Some(Ok((type_specifier, _))))) => {
      add_type_dependency(
        module,
        &module.specifier,
        type_specifier,
        type_dependencies,
      );
    }
    _ => {}
  }

  // find any @deno-types
  for dep in module.dependencies.values() {
    if let Some(type_dep) = dep.get_type() {
      if let Some(code_dep) = dep.get_code() {
        add_type_dependency(module, code_dep, type_dep, type_dependencies);
      }
    }
  }

  return Ok(());

  fn add_type_dependency(
    module: &Module,
    code_specifier: &ModuleSpecifier,
    type_specifier: &ModuleSpecifier,
    type_dependencies: &mut BTreeMap<ModuleSpecifier, HashSet<TypesDependency>>,
  ) {
    type_dependencies
      .entry(code_specifier.clone())
      .or_insert_with(HashSet::new)
      .insert(TypesDependency {
        referrer: module.specifier.clone(),
        specifier: type_specifier.clone(),
      });
  }
}
