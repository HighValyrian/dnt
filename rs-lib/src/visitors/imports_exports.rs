// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use deno_ast::swc::common::Spanned;
use deno_ast::view::*;
use deno_ast::ModuleSpecifier;
use deno_ast::TextChange;

use crate::graph::ModuleGraph;
use crate::mappings::Mappings;
use crate::utils::get_relative_specifier;

pub struct GetImportExportsTextChangesParams<'a> {
  pub specifier: &'a ModuleSpecifier,
  pub module_graph: &'a ModuleGraph,
  pub mappings: &'a Mappings,
  pub program: &'a Program<'a>,
  pub package_specifier_mappings: &'a HashMap<ModuleSpecifier, String>,
}

struct Context<'a> {
  program: &'a Program<'a>,
  specifier: &'a ModuleSpecifier,
  module_graph: &'a ModuleGraph,
  mappings: &'a Mappings,
  output_file_path: &'a PathBuf,
  text_changes: Vec<TextChange>,
  package_specifier_mappings: &'a HashMap<ModuleSpecifier, String>,
}

pub fn get_import_exports_text_changes(
  params: &GetImportExportsTextChangesParams<'_>,
) -> Result<Vec<TextChange>> {
  let mut context = Context {
    program: params.program,
    specifier: params.specifier,
    module_graph: params.module_graph,
    mappings: params.mappings,
    output_file_path: params.mappings.get_file_path(params.specifier),
    text_changes: Vec::new(),
    package_specifier_mappings: params.package_specifier_mappings,
  };

  visit_children(params.program.as_node(), &mut context)?;

  Ok(context.text_changes)
}

fn visit_children(node: Node, context: &mut Context) -> Result<()> {
  for child in node.children() {
    match child {
      Node::ImportDecl(import_decl) => {
        visit_module_specifier(import_decl.src, context);
        if let Some(asserts) = import_decl.asserts {
          visit_asserts(asserts, context);
        }
      }
      Node::ExportAll(export_all) => {
        visit_module_specifier(export_all.src, context);
        if let Some(asserts) = export_all.asserts {
          visit_asserts(asserts, context);
        }
      }
      Node::NamedExport(named_export) => {
        if let Some(src) = named_export.src.as_ref() {
          visit_module_specifier(src, context);
        }
        if let Some(asserts) = named_export.asserts {
          visit_asserts(asserts, context);
        }
      }
      Node::CallExpr(call_expr) => {
        if matches!(call_expr.callee, Callee::Import(_)) {
          if let Some(Node::Str(src)) =
            call_expr.args.get(0).map(|a| a.expr.as_node())
          {
            visit_module_specifier(src, context);
            if call_expr.args.len() > 1 {
              let assert_arg = call_expr.args[1];
              let comma_token =
                assert_arg.previous_token_fast(context.program).unwrap();
              context.text_changes.push(TextChange {
                range: (comma_token.span().lo.0 as usize)
                  ..(assert_arg.span().hi.0 as usize),
                new_text: String::new(),
              });
            }
          }
        } else {
          visit_children(child, context)?;
        }
      }
      _ => {
        visit_children(child, context)?;
      }
    }
  }

  Ok(())
}

fn visit_module_specifier(str: &Str, context: &mut Context) {
  let value = str.value().to_string();
  let specifier = context
    .module_graph
    .resolve_dependency(&value, context.specifier);
  let specifier = match specifier {
    Some(s) => s,
    None => return,
  };

  let new_text = if let Some(bare_specifier) =
    context.package_specifier_mappings.get(&specifier)
  {
    bare_specifier.to_string()
  } else {
    let specifier_file_path = context.mappings.get_file_path(&specifier);
    get_relative_specifier(context.output_file_path, specifier_file_path)
  };

  context.text_changes.push(TextChange {
    range: (str.span().lo.0 as usize + 1)..(str.span().hi.0 as usize - 1),
    new_text,
  });
}

fn visit_asserts(asserts: &ObjectLit, context: &mut Context) {
  let assert_token = asserts.previous_token_fast(context.program).unwrap();
  assert_eq!(assert_token.text_fast(context.program), "assert");
  let previous_token =
    assert_token.previous_token_fast(context.program).unwrap();
  context.text_changes.push(TextChange {
    range: (previous_token.span().hi.0 as usize)
      ..(asserts.span().hi.0 as usize),
    new_text: String::new(),
  });
}
