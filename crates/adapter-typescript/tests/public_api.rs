//! Which TypeScript items code outside the repository can really import
//! (ADR-29): a package that declares `exports` is public only through the
//! entries it lists, and every doubtful case stays public.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use smol_str::SmolStr;
use unlace_adapter_typescript::TypeScriptAdapter;
use unlace_ir::{Adapter, IrFragment, SourceFile};

/// Writes `files` under a throwaway root and binds the TypeScript sources among
/// them; manifests and `tsconfig.json` files are only written.
fn bind_project(root: &Path, files: &[(&str, &str)]) -> Result<IrFragment, String> {
    let mut sources = Vec::new();
    for (path, contents) in files {
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&target, contents).map_err(|error| error.to_string())?;
        if Path::new(path).extension().is_some_and(|ext| ext == "ts") {
            sources.push(SourceFile {
                path: SmolStr::new(*path),
                contents: (*contents).to_owned(),
            });
        }
    }
    let adapter = TypeScriptAdapter::new(root);
    let trees = adapter
        .parse(&sources)
        .map_err(|error| format!("project did not parse: {error}"))?;
    adapter
        .bind(trees)
        .map_err(|error| format!("project did not bind: {error}"))
}

/// The sorted `file:name` of every declared item the adapter marks public API;
/// re-export bindings are left out because they declare nothing of their own.
fn public_items(files: &[(&str, &str)]) -> Result<Vec<String>, String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "unlace-public-api-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let bound = bind_project(&root, files);
    let _ = fs::remove_dir_all(&root);
    let fragment = bound?;
    let mut items: Vec<String> = fragment
        .nodes
        .iter()
        .filter(|node| node.public_api && !node.re_export)
        .filter_map(|node| {
            let file = fragment
                .containers
                .iter()
                .find(|container| container.id == node.container)?;
            Some(format!("{}:{}", file.name, node.name))
        })
        .collect();
    items.sort();
    Ok(items)
}

/// A module that declares the single exported function `one`.
const ONE: &str = "export function one(): number { return 1; }\n";

#[test]
fn should_make_only_items_reachable_from_the_exports_entry_public() -> Result<(), String> {
    let public = public_items(&[
        (
            "package.json",
            r#"{"name":"pkg","exports":{".":"./src/index.ts"}}"#,
        ),
        (
            "src/index.ts",
            "export * from './a';\nexport { x } from './b';\n",
        ),
        (
            "src/a.ts",
            "export function first(): number { return 1; }\nexport const second = 2;\n",
        ),
        (
            "src/b.ts",
            "export function x(): number { return 1; }\nexport function y(): number { return 2; }\n",
        ),
        ("src/sibling.ts", ONE),
    ])?;

    assert_eq!(public, ["src/a.ts:first", "src/a.ts:second", "src/b.ts:x"]);
    Ok(())
}

#[test]
fn should_follow_re_exports_through_nested_barrels() -> Result<(), String> {
    let public = public_items(&[
        ("package.json", r#"{"exports":"./src/index.ts"}"#),
        ("src/index.ts", "export * from './mid';\n"),
        ("src/mid.ts", "export { z } from './deep';\n"),
        (
            "src/deep.ts",
            "export function z(): number { return 1; }\nexport function w(): number { return 2; }\n",
        ),
    ])?;

    assert_eq!(public, ["src/deep.ts:z"]);
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_the_package_has_no_exports_field() -> Result<(), String> {
    let public = public_items(&[
        ("package.json", r#"{"name":"pkg","main":"./src/index.ts"}"#),
        ("src/index.ts", "export { x } from './b';\n"),
        (
            "src/b.ts",
            "export function x(): number { return 1; }\nexport function y(): number { return 2; }\n",
        ),
    ])?;

    assert_eq!(public, ["src/b.ts:x", "src/b.ts:y"]);
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_there_is_no_package_manifest() -> Result<(), String> {
    let public = public_items(&[("src/a.ts", ONE)])?;

    assert_eq!(public, ["src/a.ts:one"]);
    Ok(())
}

#[test]
fn should_map_a_build_output_entry_back_to_source_through_the_tsconfig() -> Result<(), String> {
    let public = public_items(&[
        ("package.json", r#"{"exports":{".":"./lib/index.js"}}"#),
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"outDir":"./lib","rootDir":"./src"}}"#,
        ),
        ("src/index.ts", ONE),
        (
            "src/other.ts",
            "export function two(): number { return 2; }\n",
        ),
    ])?;

    assert_eq!(public, ["src/index.ts:one"]);
    Ok(())
}

#[test]
fn should_expand_a_build_output_pattern_over_the_source_files_it_maps_to() -> Result<(), String> {
    let public = public_items(&[
        (
            "package.json",
            r#"{"exports":{"./api/*":"./lib/api/*.js"}}"#,
        ),
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"outDir":"lib","rootDir":"src"}}"#,
        ),
        ("src/api/users.ts", ONE),
        ("src/api/deep/teams.ts", "export const teams = 1;\n"),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(
        public,
        ["src/api/deep/teams.ts:teams", "src/api/users.ts:one"]
    );
    Ok(())
}

#[test]
fn should_map_a_declaration_entry_through_the_declaration_directory() -> Result<(), String> {
    let public = public_items(&[
        (
            "package.json",
            r#"{"exports":{".":{"types":"./types/index.d.ts"}}}"#,
        ),
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"outDir":"dist","declarationDir":"types","rootDir":"src"}}"#,
        ),
        ("src/index.ts", ONE),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/index.ts:one"]);
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_an_entry_cannot_be_mapped_to_source() -> Result<(), String>
{
    let public = public_items(&[
        ("package.json", r#"{"exports":{".":"./dist/index.js"}}"#),
        ("src/index.ts", ONE),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/index.ts:one", "src/other.ts:other"]);
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_the_tsconfig_names_no_root_directory() -> Result<(), String>
{
    let public = public_items(&[
        ("package.json", r#"{"exports":{".":"./lib/index.js"}}"#),
        ("tsconfig.json", r#"{"compilerOptions":{"outDir":"lib"}}"#),
        ("src/index.ts", ONE),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/index.ts:one", "src/other.ts:other"]);
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_an_entry_file_is_missing_from_the_tree()
-> Result<(), String> {
    let public = public_items(&[
        ("package.json", r#"{"exports":{".":"./src/index.ts"}}"#),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/other.ts:other"]);
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_one_entry_of_several_cannot_be_mapped() -> Result<(), String>
{
    let public = public_items(&[
        (
            "package.json",
            r#"{"exports":{".":"./src/index.ts","./cli":"./dist/cli.js"}}"#,
        ),
        ("src/index.ts", ONE),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/index.ts:one", "src/other.ts:other"]);
    Ok(())
}

#[test]
fn should_expand_a_subpath_pattern_over_the_files_it_matches() -> Result<(), String> {
    let public = public_items(&[
        (
            "package.json",
            r#"{"exports":{".":"./src/index.ts","./features/*":"./src/features/*.ts"}}"#,
        ),
        ("src/index.ts", "export const root = 0;\n"),
        ("src/features/login.ts", ONE),
        ("src/features/admin/audit.ts", "export const audit = 1;\n"),
        ("src/internal.ts", "export const internal = 2;\n"),
    ])?;

    assert_eq!(
        public,
        [
            "src/features/admin/audit.ts:audit",
            "src/features/login.ts:one",
            "src/index.ts:root",
        ]
    );
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_a_pattern_matches_no_source_file() -> Result<(), String> {
    let public = public_items(&[
        ("package.json", r#"{"exports":{"./gen/*":"./gen/*.js"}}"#),
        ("src/index.ts", ONE),
    ])?;

    assert_eq!(public, ["src/index.ts:one"]);
    Ok(())
}

#[test]
fn should_count_every_condition_and_fallback_target_as_an_entry() -> Result<(), String> {
    let public = public_items(&[
        (
            "package.json",
            r#"{"exports":{
                ".":{"types":"./src/types.ts","import":{"node":"./src/esm.ts","default":"./src/web.ts"},"require":"./src/cjs.ts"},
                "./extra":["./src/extra.ts","./src/fallback.ts"]
            }}"#,
        ),
        ("src/types.ts", "export type Shape = number;\n"),
        ("src/esm.ts", "export const esm = 1;\n"),
        ("src/web.ts", "export const web = 1;\n"),
        ("src/cjs.ts", "export const cjs = 1;\n"),
        ("src/extra.ts", "export const extra = 1;\n"),
        ("src/fallback.ts", "export const fallback = 1;\n"),
        ("src/internal.ts", "export const internal = 1;\n"),
    ])?;

    assert_eq!(
        public,
        [
            "src/cjs.ts:cjs",
            "src/esm.ts:esm",
            "src/extra.ts:extra",
            "src/fallback.ts:fallback",
            "src/types.ts:Shape",
            "src/web.ts:web",
        ]
    );
    Ok(())
}

#[test]
fn should_read_a_conditions_object_directly_under_exports() -> Result<(), String> {
    let public = public_items(&[
        (
            "package.json",
            r#"{"exports":{"import":"./src/index.ts","default":"./src/index.ts"}}"#,
        ),
        ("src/index.ts", ONE),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/index.ts:one"]);
    Ok(())
}

#[test]
fn should_ignore_entries_that_are_not_code_and_blocked_entries() -> Result<(), String> {
    let public = public_items(&[
        (
            "package.json",
            r#"{"exports":{".":"./src/index.ts","./package.json":"./package.json","./styles.css":"./src/styles.css","./private/*":null}}"#,
        ),
        ("src/index.ts", ONE),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/index.ts:one"]);
    Ok(())
}

#[test]
fn should_make_nothing_public_when_exports_lists_no_code_entry() -> Result<(), String> {
    let public = public_items(&[
        (
            "package.json",
            r#"{"exports":{"./package.json":"./package.json"}}"#,
        ),
        ("src/index.ts", ONE),
    ])?;

    assert_eq!(public, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_exports_has_no_readable_shape() -> Result<(), String> {
    let public = public_items(&[("package.json", r#"{"exports":5}"#), ("src/index.ts", ONE)])?;

    assert_eq!(public, ["src/index.ts:one"]);
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_the_manifest_is_unreadable() -> Result<(), String> {
    let public = public_items(&[("package.json", "{ not json"), ("src/index.ts", ONE)])?;

    assert_eq!(public, ["src/index.ts:one"]);
    Ok(())
}

#[test]
fn should_give_each_package_of_a_monorepo_its_own_surface() -> Result<(), String> {
    let public = public_items(&[
        (
            "packages/core/package.json",
            r#"{"name":"@acme/core","exports":{".":"./src/index.ts"}}"#,
        ),
        (
            "packages/core/src/index.ts",
            "export { shown } from './shown';\n",
        ),
        ("packages/core/src/shown.ts", "export const shown = 1;\n"),
        ("packages/core/src/hidden.ts", "export const hidden = 1;\n"),
        (
            "packages/cli/package.json",
            r#"{"name":"@acme/cli","exports":{"./run":"./lib/run.js"}}"#,
        ),
        (
            "packages/cli/tsconfig.json",
            r#"{"compilerOptions":{"outDir":"lib","rootDir":"src"}}"#,
        ),
        ("packages/cli/src/run.ts", "export const run = 1;\n"),
        ("packages/cli/src/hidden.ts", "export const hidden = 1;\n"),
    ])?;

    assert_eq!(
        public,
        [
            "packages/cli/src/run.ts:run",
            "packages/core/src/shown.ts:shown"
        ]
    );
    Ok(())
}

#[test]
fn should_leave_a_package_without_exports_open_beside_a_closed_one() -> Result<(), String> {
    let public = public_items(&[
        (
            "packages/core/package.json",
            r#"{"exports":{".":"./src/index.ts"}}"#,
        ),
        ("packages/core/src/index.ts", "export const shown = 1;\n"),
        ("packages/core/src/hidden.ts", "export const hidden = 1;\n"),
        (
            "packages/legacy/package.json",
            r#"{"main":"./src/index.ts"}"#,
        ),
        ("packages/legacy/src/index.ts", "export const old = 1;\n"),
        ("packages/legacy/src/util.ts", "export const util = 1;\n"),
    ])?;

    assert_eq!(
        public,
        [
            "packages/core/src/index.ts:shown",
            "packages/legacy/src/index.ts:old",
            "packages/legacy/src/util.ts:util",
        ]
    );
    Ok(())
}

#[test]
fn should_let_the_nearest_manifest_own_a_file() -> Result<(), String> {
    let public = public_items(&[
        ("package.json", r#"{"exports":{".":"./src/index.ts"}}"#),
        ("src/index.ts", ONE),
        ("src/hidden.ts", "export const hidden = 1;\n"),
        ("tools/package.json", "{}"),
        ("tools/build.ts", "export const build = 1;\n"),
    ])?;

    assert_eq!(public, ["src/index.ts:one", "tools/build.ts:build"]);
    Ok(())
}

#[test]
fn should_make_an_item_public_when_another_package_re_exports_it() -> Result<(), String> {
    let public = public_items(&[
        (
            "packages/app/package.json",
            r#"{"exports":{".":"./src/index.ts"}}"#,
        ),
        (
            "packages/app/src/index.ts",
            "export { shared } from '../../lib/src/shared';\n",
        ),
        (
            "packages/lib/package.json",
            r#"{"exports":{".":"./src/main.ts"}}"#,
        ),
        ("packages/lib/src/main.ts", "export const main = 1;\n"),
        (
            "packages/lib/src/shared.ts",
            "export const shared = 1;\nexport const unshared = 2;\n",
        ),
    ])?;

    assert_eq!(
        public,
        [
            "packages/lib/src/main.ts:main",
            "packages/lib/src/shared.ts:shared"
        ]
    );
    Ok(())
}

#[test]
fn should_count_a_re_exported_import_a_renamed_export_and_a_default_export() -> Result<(), String> {
    let public = public_items(&[
        ("package.json", r#"{"exports":{".":"./src/index.ts"}}"#),
        (
            "src/index.ts",
            "import { a } from './a';\n\
             import { b as renamed } from './b';\n\
             import * as everything from './c';\n\
             import fallback from './d';\n\
             function local(): number { return 1; }\n\
             function chosen(): number { return 2; }\n\
             export { a, renamed as bee, local as home, everything };\n\
             export default chosen;\n\
             export { fallback };\n",
        ),
        (
            "src/a.ts",
            "export function a(): number { return 1; }\nexport function a2(): number { return 2; }\n",
        ),
        (
            "src/b.ts",
            "export function b(): number { return 1; }\nexport function b2(): number { return 2; }\n",
        ),
        ("src/c.ts", "export const c1 = 1;\nexport const c2 = 2;\n"),
        (
            "src/d.ts",
            "export default function d(): number { return 1; }\n",
        ),
        ("src/unrelated.ts", "export const unrelated = 1;\n"),
    ])?;

    assert_eq!(
        public,
        [
            "src/a.ts:a",
            "src/b.ts:b",
            "src/c.ts:c1",
            "src/c.ts:c2",
            "src/d.ts:d",
            "src/index.ts:chosen",
            "src/index.ts:local",
        ]
    );
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_an_entry_clause_names_nothing_it_can_find()
-> Result<(), String> {
    let public = public_items(&[
        ("package.json", r#"{"exports":{".":"./src/index.ts"}}"#),
        ("src/index.ts", "export { ghost };\n"),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/other.ts:other"]);
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_a_reached_module_re_exports_a_file_it_cannot_resolve()
-> Result<(), String> {
    let public = public_items(&[
        ("package.json", r#"{"exports":{".":"./src/index.ts"}}"#),
        (
            "src/index.ts",
            "export * from './gone.js';\nexport * from 'zod';\n",
        ),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/other.ts:other"]);
    Ok(())
}

#[test]
fn should_stay_closed_when_a_reached_module_re_exports_an_external_package() -> Result<(), String> {
    let public = public_items(&[
        ("package.json", r#"{"exports":{".":"./src/index.ts"}}"#),
        (
            "src/index.ts",
            "export * from 'zod';\nexport const own = 1;\n",
        ),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/index.ts:own"]);
    Ok(())
}

/// A package whose only entry is `src/index.ts`.
const ENTRY_ONLY: (&str, &str) = ("package.json", r#"{"exports":"./src/index.ts"}"#);

#[test]
fn should_follow_a_re_exported_import_inside_a_reached_module() -> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        ("src/index.ts", "export * from './middle';\n"),
        ("src/middle.ts", "import { a } from './a';\nexport { a };\n"),
        (
            "src/a.ts",
            "export function a(): number { return 1; }\nexport function a2(): number { return 2; }\n",
        ),
    ])?;

    assert_eq!(public, ["src/a.ts:a"]);
    Ok(())
}

#[test]
fn should_follow_a_default_re_export_to_the_default_clause_of_its_source() -> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        ("src/index.ts", "export { default as foo } from './foo';\n"),
        (
            "src/foo.ts",
            "export function foo(): number { return 1; }\nexport default foo;\nexport const extra = 1;\n",
        ),
    ])?;

    assert_eq!(public, ["src/foo.ts:foo"]);
    Ok(())
}

#[test]
fn should_follow_the_names_an_object_default_export_mentions() -> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        (
            "src/index.ts",
            "import { foo } from './foo';\nexport default { foo, other: 1 };\n",
        ),
        (
            "src/foo.ts",
            "export function foo(): number { return 1; }\nexport function unused(): number { return 2; }\n",
        ),
    ])?;

    assert_eq!(public, ["src/foo.ts:foo"]);
    Ok(())
}

#[test]
fn should_follow_the_names_a_wrapped_default_export_mentions() -> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        (
            "src/index.ts",
            "import { make } from './make';\nimport { Base } from './base';\nexport default make(Base);\n",
        ),
        (
            "src/make.ts",
            "export function make(base: unknown): unknown { return base; }\n",
        ),
        (
            "src/base.ts",
            "export const Base = 1;\nexport const Spare = 2;\n",
        ),
    ])?;

    assert_eq!(public, ["src/base.ts:Base", "src/make.ts:make"]);
    Ok(())
}

#[test]
fn should_make_an_item_public_when_a_package_without_exports_re_exports_it() -> Result<(), String> {
    let public = public_items(&[
        (
            "packages/a/package.json",
            r#"{"exports":{".":"./src/index.ts"}}"#,
        ),
        ("packages/a/src/index.ts", "export const shown = 1;\n"),
        ("packages/a/src/internal.ts", "export const inner = 1;\n"),
        ("packages/a/src/hidden.ts", "export const hidden = 1;\n"),
        ("packages/b/package.json", r#"{"main":"./src/barrel.ts"}"#),
        (
            "packages/b/src/barrel.ts",
            "export * from '../../a/src/internal';\n",
        ),
    ])?;

    assert_eq!(
        public,
        [
            "packages/a/src/index.ts:shown",
            "packages/a/src/internal.ts:inner"
        ]
    );
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_a_package_without_exports_re_exports_an_unresolved_file()
-> Result<(), String> {
    let public = public_items(&[
        (
            "packages/a/package.json",
            r#"{"exports":{".":"./src/index.ts"}}"#,
        ),
        ("packages/a/src/index.ts", "export const shown = 1;\n"),
        ("packages/a/src/hidden.ts", "export const hidden = 1;\n"),
        ("packages/b/package.json", "{}"),
        (
            "packages/b/src/barrel.ts",
            "export * from '../../a/src/hidden.js';\n",
        ),
    ])?;

    assert_eq!(
        public,
        [
            "packages/a/src/hidden.ts:hidden",
            "packages/a/src/index.ts:shown"
        ]
    );
    Ok(())
}

#[test]
fn should_follow_a_default_export_of_a_declared_local_through_a_reached_module()
-> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        (
            "src/index.ts",
            "export { default as Button } from './Button';\n",
        ),
        (
            "src/Button.ts",
            "const Button = 1;\nexport default Button;\nexport const Spare = 2;\n",
        ),
    ])?;

    assert_eq!(public, ["src/Button.ts:Button"]);
    Ok(())
}

#[test]
fn should_follow_a_local_export_list_inside_a_reached_module() -> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        ("src/index.ts", "export * from './middle';\n"),
        (
            "src/middle.ts",
            "function m(): number { return 1; }\nfunction n(): number { return 2; }\nexport { m };\n",
        ),
    ])?;

    assert_eq!(public, ["src/middle.ts:m"]);
    Ok(())
}

#[test]
fn should_follow_a_type_only_passthrough_inside_a_reached_module() -> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        ("src/index.ts", "export { T } from './middle';\n"),
        (
            "src/middle.ts",
            "import type { T } from './t';\nexport type { T };\n",
        ),
        (
            "src/t.ts",
            "export interface T { a: number }\nexport interface U { b: number }\n",
        ),
    ])?;

    assert_eq!(public, ["src/t.ts:T"]);
    Ok(())
}

#[test]
fn should_follow_a_renamed_export_list_entry_by_its_exported_name() -> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        ("src/index.ts", "export { wanted } from './middle';\n"),
        (
            "src/middle.ts",
            "function a(): number { return 1; }\nfunction b(): number { return 2; }\nexport { a as wanted, b as other };\n",
        ),
    ])?;

    assert_eq!(public, ["src/middle.ts:a"]);
    Ok(())
}

#[test]
fn should_still_close_the_siblings_of_a_chain_it_can_follow_to_the_end() -> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        (
            "src/index.ts",
            "export { default as foo } from './foo';\nexport * from './middle';\nexport { x } from './barrel';\n",
        ),
        (
            "src/foo.ts",
            "function foo(): number { return 1; }\nexport default foo;\nexport const fooSpare = 1;\n",
        ),
        (
            "src/middle.ts",
            "import { a } from './a';\nfunction m(): number { return 1; }\nfunction n(): number { return 2; }\nexport { a, m };\n",
        ),
        (
            "src/a.ts",
            "export function a(): number { return 1; }\nexport function aSpare(): number { return 2; }\n",
        ),
        (
            "src/barrel.ts",
            "export * from './left';\nexport * from './right';\n",
        ),
        ("src/left.ts", "export const leftOnly = 1;\n"),
        (
            "src/right.ts",
            "export const x = 1;\nexport const rightSpare = 2;\n",
        ),
        ("src/unreached.ts", "export const unreached = 1;\n"),
    ])?;

    assert_eq!(
        public,
        [
            "src/a.ts:a",
            "src/foo.ts:foo",
            "src/middle.ts:m",
            "src/right.ts:x"
        ]
    );
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_a_named_re_export_finds_no_such_export()
-> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        ("src/index.ts", "export { gone } from './middle';\n"),
        ("src/middle.ts", "export const present = 1;\n"),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/middle.ts:present", "src/other.ts:other"]);
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_an_entry_has_an_extension_it_cannot_read()
-> Result<(), String> {
    let public = public_items(&[
        (
            "package.json",
            r#"{"exports":{".":"./src/index.ts","./comp":"./src/Comp.vue"}}"#,
        ),
        ("src/index.ts", ONE),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/index.ts:one", "src/other.ts:other"]);
    Ok(())
}

#[test]
fn should_follow_the_module_an_exported_import_equals_requires() -> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        ("src/index.ts", "export import a = require('./a');\n"),
        (
            "src/a.ts",
            "export function foo(): number { return 1; }\nexport function bar(): number { return 2; }\n",
        ),
        ("src/unreached.ts", "export const unreached = 1;\n"),
    ])?;

    assert_eq!(public, ["src/a.ts:bar", "src/a.ts:foo"]);
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_an_exported_import_equals_names_an_entity()
-> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        (
            "src/index.ts",
            "namespace ns { export const y = 1; }\nexport import x = ns.y;\n",
        ),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/other.ts:other"]);
    Ok(())
}

#[test]
fn should_follow_a_dynamic_import_inside_a_default_export() -> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        ("src/index.ts", "export default await import('./impl');\n"),
        (
            "src/impl.ts",
            "export function foo(): number { return 1; }\nexport function bar(): number { return 2; }\n",
        ),
        ("src/unreached.ts", "export const unreached = 1;\n"),
    ])?;

    assert_eq!(public, ["src/impl.ts:bar", "src/impl.ts:foo"]);
    Ok(())
}

#[test]
fn should_follow_a_require_inside_an_export_assignment() -> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        ("src/index.ts", "export = { impl: require('./impl') };\n"),
        (
            "src/impl.ts",
            "export function foo(): number { return 1; }\nexport function bar(): number { return 2; }\n",
        ),
        ("src/unreached.ts", "export const unreached = 1;\n"),
    ])?;

    assert_eq!(public, ["src/impl.ts:bar", "src/impl.ts:foo"]);
    Ok(())
}

#[test]
fn should_keep_every_export_public_when_an_exported_expression_loads_a_computed_module()
-> Result<(), String> {
    let dynamic = public_items(&[
        ENTRY_ONLY,
        (
            "src/index.ts",
            "declare const name: string;\nexport default await import(name);\n",
        ),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;
    let required = public_items(&[
        ENTRY_ONLY,
        (
            "src/index.ts",
            "declare const name: string;\nexport = require(name);\n",
        ),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(dynamic, ["src/other.ts:other"]);
    assert_eq!(required, ["src/other.ts:other"]);
    Ok(())
}

/// A closed package `@x/a` with one reachable and one hidden item.
const CLOSED_A: [(&str, &str); 3] = [
    (
        "packages/a/package.json",
        r#"{"name":"@x/a","exports":{".":"./src/index.ts"}}"#,
    ),
    ("packages/a/src/index.ts", "export const shown = 1;\n"),
    ("packages/a/src/hidden.ts", "export const hidden = 1;\n"),
];

/// `CLOSED_A` plus `extra` files.
fn beside_closed_a<'files>(
    extra: &[(&'files str, &'files str)],
) -> Vec<(&'files str, &'files str)> {
    CLOSED_A
        .iter()
        .copied()
        .chain(extra.iter().copied())
        .collect()
}

#[test]
fn should_keep_a_clean_package_closed_beside_an_open_one_that_re_exports_a_js_specifier()
-> Result<(), String> {
    let public = public_items(&beside_closed_a(&[
        ("packages/app/package.json", "{}"),
        ("packages/app/src/main.ts", "export * from './util.js';\n"),
        ("packages/app/src/app.ts", "export const app = 1;\n"),
    ]))?;

    assert_eq!(
        public,
        [
            "packages/a/src/index.ts:shown",
            "packages/app/src/app.ts:app"
        ]
    );
    Ok(())
}

#[test]
fn should_keep_a_clean_package_closed_beside_an_open_one_that_re_exports_an_unreadable_file()
-> Result<(), String> {
    let public = public_items(&beside_closed_a(&[
        ("packages/app/package.json", "{}"),
        (
            "packages/app/src/main.ts",
            "export { default as C } from './C.vue';\n",
        ),
        ("packages/app/src/app.ts", "export const app = 1;\n"),
    ]))?;

    assert_eq!(
        public,
        [
            "packages/a/src/index.ts:shown",
            "packages/app/src/app.ts:app"
        ]
    );
    Ok(())
}

#[test]
fn should_keep_a_clean_package_closed_beside_a_file_in_no_package_that_re_exports_a_js_specifier()
-> Result<(), String> {
    let public = public_items(&beside_closed_a(&[(
        "tools/run.ts",
        "export * from './gone.js';\nexport const run = 1;\n",
    )]))?;

    assert_eq!(
        public,
        ["packages/a/src/index.ts:shown", "tools/run.ts:run"]
    );
    Ok(())
}

#[test]
fn should_reopen_a_closed_package_that_an_open_one_re_exports_through_an_unresolved_path()
-> Result<(), String> {
    let public = public_items(&beside_closed_a(&[
        ("packages/b/package.json", "{}"),
        (
            "packages/b/src/barrel.ts",
            "export * from '../../a/src/hidden.js';\n",
        ),
    ]))?;

    assert_eq!(
        public,
        [
            "packages/a/src/hidden.ts:hidden",
            "packages/a/src/index.ts:shown"
        ]
    );
    Ok(())
}

#[test]
fn should_reopen_a_closed_package_that_an_open_one_re_exports_by_package_name() -> Result<(), String>
{
    let public = public_items(&beside_closed_a(&[
        ("packages/b/package.json", "{}"),
        (
            "packages/b/src/barrel.ts",
            "export * from '@x/a/internal';\n",
        ),
    ]))?;

    assert_eq!(
        public,
        [
            "packages/a/src/hidden.ts:hidden",
            "packages/a/src/index.ts:shown"
        ]
    );
    Ok(())
}

#[test]
fn should_reopen_a_closed_package_that_an_open_one_re_exports_by_subpath_import()
-> Result<(), String> {
    let public = public_items(&beside_closed_a(&[
        ("packages/b/package.json", "{}"),
        ("packages/b/src/barrel.ts", "export * from '#missing';\n"),
    ]))?;

    assert_eq!(
        public,
        [
            "packages/a/src/hidden.ts:hidden",
            "packages/a/src/index.ts:shown"
        ]
    );
    Ok(())
}

/// A closed package whose manifest also names `src/secret.ts` as its `types`.
const CLOSED_WITH_TYPES: [(&str, &str); 4] = [
    ("package.json", r#"{"private":true}"#),
    (
        "packages/closed/package.json",
        r#"{"exports":"./src/index.ts","types":"./src/secret.ts"}"#,
    ),
    ("packages/closed/src/index.ts", "export const shown = 1;\n"),
    (
        "packages/closed/src/secret.ts",
        "export const secret = 1;\n",
    ),
];

/// `CLOSED_WITH_TYPES` plus `extra` files.
fn beside_closed_types<'files>(
    extra: &[(&'files str, &'files str)],
) -> Vec<(&'files str, &'files str)> {
    CLOSED_WITH_TYPES
        .iter()
        .copied()
        .chain(extra.iter().copied())
        .collect()
}

#[test]
fn should_reopen_a_closed_package_that_an_open_one_re_exports_by_its_folder() -> Result<(), String>
{
    let public = public_items(&beside_closed_types(&[
        ("packages/app/package.json", "{}"),
        (
            "packages/app/src/main.ts",
            "export * from '../../closed';\n",
        ),
    ]))?;

    assert_eq!(
        public,
        [
            "packages/closed/src/index.ts:shown",
            "packages/closed/src/secret.ts:secret"
        ]
    );
    Ok(())
}

#[test]
fn should_reopen_a_closed_package_that_an_open_one_re_exports_by_its_folder_with_a_slash()
-> Result<(), String> {
    let public = public_items(&beside_closed_types(&[
        ("packages/app/package.json", "{}"),
        (
            "packages/app/src/main.ts",
            "export * from '../../closed/';\n",
        ),
    ]))?;

    assert_eq!(
        public,
        [
            "packages/closed/src/index.ts:shown",
            "packages/closed/src/secret.ts:secret"
        ]
    );
    Ok(())
}

#[test]
fn should_keep_a_closed_package_closed_beside_an_open_one_that_re_exports_another_open_folder()
-> Result<(), String> {
    let public = public_items(&beside_closed_types(&[
        ("packages/app/package.json", "{}"),
        ("packages/app/src/main.ts", "export * from '../../other';\n"),
        ("packages/other/package.json", "{}"),
        ("packages/other/src/o.ts", "export const o = 1;\n"),
    ]))?;

    assert_eq!(
        public,
        [
            "packages/closed/src/index.ts:shown",
            "packages/other/src/o.ts:o"
        ]
    );
    Ok(())
}

/// A root `tsconfig.json` that maps `@/*` to `src/*`.
const ALIAS_AT: (&str, &str) = (
    "tsconfig.json",
    r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["src/*"]}}}"#,
);

#[test]
fn should_keep_every_export_public_when_an_alias_re_export_does_not_resolve() -> Result<(), String>
{
    let js = public_items(&[
        ENTRY_ONLY,
        ALIAS_AT,
        ("src/index.ts", "export * from '@/internal.js';\n"),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;
    let vue = public_items(&[
        ENTRY_ONLY,
        ALIAS_AT,
        ("src/index.ts", "export { default as C } from '@/C.vue';\n"),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(js, ["src/other.ts:other"]);
    assert_eq!(vue, ["src/other.ts:other"]);
    Ok(())
}

#[test]
fn should_follow_an_alias_re_export_that_resolves_and_still_close_its_siblings()
-> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        ALIAS_AT,
        ("src/index.ts", "export * from '@/lib';\n"),
        ("src/lib.ts", "export const lib = 1;\n"),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/lib.ts:lib"]);
    Ok(())
}

#[test]
fn should_stay_closed_when_a_package_outside_the_aliases_is_re_exported() -> Result<(), String> {
    let public = public_items(&[
        ENTRY_ONLY,
        ALIAS_AT,
        (
            "src/index.ts",
            "export * from 'zod';\nexport const own = 1;\n",
        ),
        ("src/other.ts", "export const other = 2;\n"),
    ])?;

    assert_eq!(public, ["src/index.ts:own"]);
    Ok(())
}
