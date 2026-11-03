//! Which Python items code outside the repository can import (ADR-29, rules 5
//! and 6): a leading underscore on a name, module or package, and a module's
//! `__all__`, mark an item private; every doubtful case stays public.

use smol_str::SmolStr;
use unlace_adapter_python::PythonAdapter;
use unlace_ir::{Adapter, SourceFile};

/// The sorted `file:name` of every node the adapter marks public API,
/// re-export bindings included.
fn public_items(files: &[(&str, &str)]) -> Result<Vec<String>, String> {
    let sources: Vec<SourceFile> = files
        .iter()
        .map(|(path, contents)| SourceFile {
            path: SmolStr::new(*path),
            contents: (*contents).to_owned(),
        })
        .collect();
    let adapter = PythonAdapter::new(".");
    let trees = adapter
        .parse(&sources)
        .map_err(|error| format!("project did not parse: {error}"))?;
    let fragment = adapter
        .bind(trees)
        .map_err(|error| format!("project did not bind: {error}"))?;
    let mut items: Vec<String> = fragment
        .nodes
        .iter()
        .filter(|node| node.public_api)
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

#[test]
fn should_keep_a_dunder_name_public_and_an_underscore_name_private() -> Result<(), String> {
    let public = public_items(&[(
        "pkg/meta.py",
        "__version__ = \"1\"\n\n\ndef shown() -> int:\n    return 1\n\n\ndef _hidden() -> int:\n    return 2\n\n\ndef __mangled() -> int:\n    return 3\n",
    )])?;

    assert_eq!(public, ["pkg/meta.py:__version__", "pkg/meta.py:shown"]);
    Ok(())
}

#[test]
fn should_make_items_of_an_underscore_module_private() -> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.py", ""),
        ("pkg/_helpers.py", "def assist() -> int:\n    return 1\n"),
        ("pkg/api.py", "def serve() -> int:\n    return 2\n"),
    ])?;

    assert_eq!(public, ["pkg/api.py:serve"]);
    Ok(())
}

#[test]
fn should_make_items_below_an_underscore_package_private() -> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.py", ""),
        ("pkg/_internal/__init__.py", ""),
        ("pkg/_internal/deep/__init__.py", ""),
        (
            "pkg/_internal/deep/work.py",
            "def labour() -> int:\n    return 1\n",
        ),
        ("pkg/api.py", "def serve() -> int:\n    return 2\n"),
    ])?;

    assert_eq!(public, ["pkg/api.py:serve"]);
    Ok(())
}

#[test]
fn should_keep_the_items_of_a_dunder_module_public() -> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.py", ""),
        ("pkg/__about__.py", "__version__ = \"1\"\n"),
    ])?;

    assert_eq!(public, ["pkg/__about__.py:__version__"]);
    Ok(())
}

#[test]
fn should_make_a_name_public_only_when_the_module_all_lists_it() -> Result<(), String> {
    let public = public_items(&[(
        "pkg/api.py",
        "__all__ = [\"listed\"]\n\n\ndef listed() -> int:\n    return 1\n\n\ndef omitted() -> int:\n    return 2\n",
    )])?;

    assert_eq!(public, ["pkg/api.py:listed"]);
    Ok(())
}

#[test]
fn should_make_every_name_private_when_the_module_all_is_empty() -> Result<(), String> {
    let public = public_items(&[(
        "pkg/api.py",
        "__all__ = []\n\n\ndef omitted() -> int:\n    return 1\n",
    )])?;

    assert_eq!(public, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_keep_every_non_underscore_name_public_when_all_grows_with_an_augmented_assignment()
-> Result<(), String> {
    let public = public_items(&[(
        "pkg/api.py",
        "__all__ = [\"first\"]\n__all__ += [\"second\"]\n\n\ndef first() -> int:\n    return 1\n\n\ndef second() -> int:\n    return 2\n\n\ndef _third() -> int:\n    return 3\n",
    )])?;

    assert_eq!(public, ["pkg/api.py:first", "pkg/api.py:second"]);
    Ok(())
}

#[test]
fn should_keep_every_non_underscore_name_public_when_all_is_a_comprehension() -> Result<(), String>
{
    let public = public_items(&[(
        "pkg/api.py",
        "__all__ = [name for name in dir() if not name.startswith(\"_\")]\n\n\ndef first() -> int:\n    return 1\n\n\ndef second() -> int:\n    return 2\n",
    )])?;

    assert_eq!(public, ["pkg/api.py:first", "pkg/api.py:second"]);
    Ok(())
}

#[test]
fn should_keep_every_non_underscore_name_public_when_all_is_set_in_a_conditional_block()
-> Result<(), String> {
    let public = public_items(&[(
        "pkg/api.py",
        "import sys\n\n__all__ = [\"first\"]\nif sys.platform == \"win32\":\n    __all__.append(\"second\")\n\n\ndef first() -> int:\n    return 1\n\n\ndef second() -> int:\n    return 2\n",
    )])?;

    assert_eq!(public, ["pkg/api.py:first", "pkg/api.py:second"]);
    Ok(())
}

#[test]
fn should_publish_an_item_of_a_private_module_that_a_package_init_re_exports() -> Result<(), String>
{
    let public = public_items(&[
        ("pkg/__init__.py", "from ._private import Widget\n"),
        (
            "pkg/_private.py",
            "class Widget:\n    pass\n\n\nclass Hidden:\n    pass\n",
        ),
    ])?;

    assert_eq!(public, ["pkg/__init__.py:Widget", "pkg/_private.py:Widget"]);
    Ok(())
}

#[test]
fn should_not_publish_a_re_export_the_package_init_all_leaves_out() -> Result<(), String> {
    let public = public_items(&[
        (
            "pkg/__init__.py",
            "from ._parts import Kept, Dropped\n\n__all__ = [\"Kept\"]\n",
        ),
        (
            "pkg/_parts.py",
            "class Kept:\n    pass\n\n\nclass Dropped:\n    pass\n",
        ),
    ])?;

    assert_eq!(public, ["pkg/__init__.py:Kept", "pkg/_parts.py:Kept"]);
    Ok(())
}

#[test]
fn should_not_publish_a_re_export_of_an_underscore_name() -> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.py", "from .parts import _internal, shown\n"),
        (
            "pkg/parts.py",
            "def shown() -> int:\n    return 1\n\n\ndef _internal() -> int:\n    return 2\n",
        ),
    ])?;

    assert_eq!(public, ["pkg/__init__.py:shown", "pkg/parts.py:shown"]);
    Ok(())
}

#[test]
fn should_not_publish_through_a_package_init_that_is_itself_private() -> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.py", ""),
        ("pkg/_internal/__init__.py", "from .work import labour\n"),
        (
            "pkg/_internal/work.py",
            "def labour() -> int:\n    return 1\n",
        ),
    ])?;

    assert_eq!(public, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_publish_an_item_that_a_public_init_reaches_through_a_private_init() -> Result<(), String>
{
    // files in the sorted order the engine feeds the adapter: the outer package
    // is bound before the inner one that it re-exports from.
    let public = public_items(&[
        ("pkg/__init__.py", "from ._mid import Core\n"),
        ("pkg/_mid/__init__.py", "from .core import Core\n"),
        ("pkg/_mid/core.py", "class Core:\n    pass\n"),
    ])?;

    assert_eq!(
        public,
        [
            "pkg/__init__.py:Core",
            "pkg/_mid/__init__.py:Core",
            "pkg/_mid/core.py:Core"
        ]
    );
    Ok(())
}

#[test]
fn should_keep_a_dunder_name_public_when_the_module_all_leaves_it_out() -> Result<(), String> {
    let public = public_items(&[(
        "pkg/meta.py",
        "__all__ = [\"shown\"]\n__version__ = \"1\"\n\n\ndef shown() -> int:\n    return 1\n\n\ndef other() -> int:\n    return 2\n",
    )])?;

    assert_eq!(public, ["pkg/meta.py:__version__", "pkg/meta.py:shown"]);
    Ok(())
}

#[test]
fn should_publish_every_public_name_a_star_re_export_reaches_when_all_is_not_literal()
-> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.py", "from ._core import *\n"),
        (
            "pkg/_core.py",
            "__all__ = [\"a\"]\n__all__ += [\"b\"]\n\n\ndef a() -> int:\n    return 1\n\n\ndef b() -> int:\n    return 2\n\n\ndef _c() -> int:\n    return 3\n",
        ),
    ])?;

    assert_eq!(
        public,
        [
            "pkg/__init__.py:a",
            "pkg/__init__.py:b",
            "pkg/_core.py:a",
            "pkg/_core.py:b"
        ]
    );
    Ok(())
}

#[test]
fn should_publish_only_the_listed_names_a_star_re_export_reaches() -> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.py", "from ._core import *\n"),
        (
            "pkg/_core.py",
            "__all__ = [\"a\"]\n\n\ndef a() -> int:\n    return 1\n\n\ndef b() -> int:\n    return 2\n",
        ),
    ])?;

    assert_eq!(public, ["pkg/__init__.py:a", "pkg/_core.py:a"]);
    Ok(())
}

#[test]
fn should_publish_the_names_a_public_module_star_imports() -> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.py", ""),
        ("pkg/api.py", "from ._impl import *\n"),
        (
            "pkg/_impl.py",
            "def a() -> int:\n    return 1\n\n\ndef _b() -> int:\n    return 2\n",
        ),
    ])?;

    assert_eq!(public, ["pkg/_impl.py:a"]);
    Ok(())
}

#[test]
fn should_publish_an_item_that_reaches_a_package_init_through_a_plain_module() -> Result<(), String>
{
    let public = public_items(&[
        ("pkg/__init__.py", "from ._api import Widget\n"),
        ("pkg/_api.py", "from ._impl import Widget\n"),
        ("pkg/_impl.py", "class Widget:\n    pass\n"),
    ])?;

    assert_eq!(public, ["pkg/_impl.py:Widget"]);
    Ok(())
}

#[test]
fn should_publish_what_any_public_module_imports_and_lists() -> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.py", ""),
        (
            "pkg/api.py",
            "from ._impl import Widget\n\n__all__ = [\"Widget\"]\n",
        ),
        ("pkg/_impl.py", "class Widget:\n    pass\n"),
        (
            "pkg/quiet.py",
            "from ._impl2 import Gadget\n\n__all__ = []\n",
        ),
        ("pkg/_impl2.py", "class Gadget:\n    pass\n"),
    ])?;

    assert_eq!(public, ["pkg/_impl.py:Widget"]);
    Ok(())
}

#[test]
fn should_publish_what_a_package_init_imports_inside_try_if_and_type_checking_blocks()
-> Result<(), String> {
    let public = public_items(&[
        (
            "pkg/__init__.py",
            "try:\n    from ._speedups import fast\nexcept ImportError:\n    from ._pure import fast\n\nif TYPE_CHECKING:\n    from ._types import Alias\n",
        ),
        ("pkg/_pure.py", "def fast() -> int:\n    return 1\n"),
        ("pkg/_speedups.py", "def fast() -> int:\n    return 2\n"),
        ("pkg/_types.py", "class Alias:\n    pass\n"),
    ])?;

    assert_eq!(
        public,
        [
            "pkg/_pure.py:fast",
            "pkg/_speedups.py:fast",
            "pkg/_types.py:Alias"
        ]
    );
    Ok(())
}

#[test]
fn should_keep_every_non_underscore_name_public_when_a_function_extends_all() -> Result<(), String>
{
    let public = public_items(&[(
        "pkg/api.py",
        "__all__ = []\n\n\ndef export(fn):\n    __all__.append(fn.__name__)\n    return fn\n\n\n@export\ndef thing() -> int:\n    return 1\n\n\ndef _hidden() -> int:\n    return 2\n",
    )])?;

    assert_eq!(public, ["pkg/api.py:export", "pkg/api.py:thing"]);
    Ok(())
}

#[test]
fn should_count_underscore_segments_from_the_import_root_down() -> Result<(), String> {
    let public = public_items(&[
        ("_legacy/proj/pkg/__init__.py", ""),
        (
            "_legacy/proj/pkg/mod.py",
            "def run() -> int:\n    return 1\n",
        ),
        (
            "_legacy/proj/pkg/_hidden.py",
            "def inner() -> int:\n    return 2\n",
        ),
        ("_scripts/tool.py", "def go() -> int:\n    return 3\n"),
        ("_loose.py", "def lone() -> int:\n    return 4\n"),
    ])?;

    assert_eq!(
        public,
        ["_legacy/proj/pkg/mod.py:run", "_scripts/tool.py:go"]
    );
    Ok(())
}

#[test]
fn should_follow_an_absolute_self_import_of_a_src_layout() -> Result<(), String> {
    let public = public_items(&[
        ("src/pkg/__init__.py", "from pkg._impl import Widget\n"),
        ("src/pkg/_impl.py", "class Widget:\n    pass\n"),
        ("src/pkg/api.py", "def serve() -> int:\n    return 1\n"),
    ])?;

    assert_eq!(public, ["src/pkg/_impl.py:Widget", "src/pkg/api.py:serve"]);
    Ok(())
}

#[test]
fn should_read_stub_files_like_sources_for_dunder_and_module_names() -> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.pyi", "def f() -> int: ...\n"),
        ("pkg/mod.pyi", "def g() -> int: ...\n"),
        ("pkg/_priv.pyi", "def h() -> int: ...\n"),
    ])?;

    assert_eq!(public, ["pkg/__init__.pyi:f", "pkg/mod.pyi:g"]);
    Ok(())
}

#[test]
fn should_keep_the_items_below_a_lazy_package_init_public() -> Result<(), String> {
    let public = public_items(&[
        (
            "pkg/__init__.py",
            "def __getattr__(name: str) -> object:\n    raise AttributeError(name)\n",
        ),
        (
            "pkg/_lazy.py",
            "class Heavy:\n    pass\n\n\ndef _hidden() -> int:\n    return 1\n",
        ),
        ("other/__init__.py", ""),
        ("other/_eager.py", "class Light:\n    pass\n"),
    ])?;

    assert_eq!(
        public,
        ["pkg/__init__.py:__getattr__", "pkg/_lazy.py:Heavy"]
    );
    Ok(())
}

#[test]
fn should_not_publish_what_only_test_code_imports() -> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.py", ""),
        ("pkg/_helpers.py", "def assist() -> int:\n    return 1\n"),
        ("pkg/_more.py", "def extra() -> int:\n    return 2\n"),
        (
            "pkg/test_api.py",
            "from pkg._helpers import assist\n\n\ndef test_it() -> None:\n    assert assist()\n",
        ),
        ("tests/conftest.py", "from pkg._more import extra\n"),
    ])?;

    assert_eq!(
        public,
        ["pkg/test_api.py:test_it"],
        "an import in test code makes nothing available to outside callers"
    );
    Ok(())
}

#[test]
fn should_follow_self_imports_when_the_analysed_root_is_itself_a_package() -> Result<(), String> {
    let public = public_items(&[
        ("__init__.py", "from ._impl import Widget\n"),
        ("_impl.py", "class Widget:\n    pass\n"),
        ("api.py", "from pkg._impl2 import Gadget\n"),
        ("_impl2.py", "class Gadget:\n    pass\n"),
    ])?;

    assert_eq!(
        public,
        ["__init__.py:Widget", "_impl.py:Widget", "_impl2.py:Gadget"]
    );
    Ok(())
}

#[test]
fn should_publish_what_a_public_module_imports_from_a_stub() -> Result<(), String> {
    // a stub initializer binds no re-export node (stubs take no dependency
    // edges), but the item it imports is still public API.
    let stubs = public_items(&[
        ("pkg/__init__.pyi", "from ._impl import Widget as Widget\n"),
        ("pkg/_impl.pyi", "class Widget: ...\n"),
    ])?;
    let mixed = public_items(&[
        ("pkg/__init__.pyi", "from ._impl import Widget as Widget\n"),
        ("pkg/_impl.py", "class Widget:\n    pass\n"),
    ])?;

    assert_eq!(stubs, ["pkg/_impl.pyi:Widget"]);
    assert_eq!(mixed, ["pkg/_impl.py:Widget"]);
    Ok(())
}

#[test]
fn should_keep_a_package_public_when_its_init_imports_a_getattr_hook() -> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.py", "from ._lazy import __getattr__\n"),
        (
            "pkg/_lazy.py",
            "def __getattr__(name: str) -> object:\n    raise AttributeError(name)\n",
        ),
        ("pkg/_heavy.py", "class Heavy:\n    pass\n"),
    ])?;

    assert_eq!(
        public,
        [
            "pkg/__init__.py:__getattr__",
            "pkg/_heavy.py:Heavy",
            "pkg/_lazy.py:__getattr__"
        ]
    );
    Ok(())
}

#[test]
fn should_keep_a_package_public_when_its_getattr_hook_sits_in_a_conditional_block()
-> Result<(), String> {
    let defined = public_items(&[
        (
            "pkg/__init__.py",
            "import sys\n\nif sys.version_info >= (3, 7):\n\n    def __getattr__(name: str) -> object:\n        raise AttributeError(name)\n",
        ),
        ("pkg/_heavy.py", "class Heavy:\n    pass\n"),
    ])?;
    let imported = public_items(&[
        (
            "pkg/__init__.py",
            "try:\n    from ._lazy import __getattr__ as __getattr__\nexcept ImportError:\n    pass\n",
        ),
        ("pkg/_heavy.py", "class Heavy:\n    pass\n"),
    ])?;

    assert_eq!(defined, ["pkg/_heavy.py:Heavy"]);
    assert_eq!(imported, ["pkg/_heavy.py:Heavy"]);
    Ok(())
}

#[test]
fn should_match_test_paths_on_whole_segments() -> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.py", ""),
        ("pkg/_impl.py", "class Widget:\n    pass\n"),
        ("pkg/_more.py", "class Gadget:\n    pass\n"),
        ("pkg/contests/__init__.py", "from .._impl import Widget\n"),
        ("pkg/tests/__init__.py", "from .._more import Gadget\n"),
        ("pkg/helpers_test.py", "from ._more import Gadget\n"),
        ("pkg/test_helpers.py", "from ._more import Gadget\n"),
    ])?;

    let private_modules: Vec<&String> = public
        .iter()
        .filter(|item| item.starts_with("pkg/_"))
        .collect();
    assert_eq!(private_modules, ["pkg/_impl.py:Widget"]);
    Ok(())
}

#[test]
fn should_publish_what_a_shipped_test_package_imports() -> Result<(), String> {
    // `django.test` is production code: only a module that both the folder name
    // and the polarity rules call test code is skipped.
    let public = public_items(&[
        ("pkg/__init__.py", ""),
        ("pkg/_fakes.py", "class FakeClient:\n    pass\n"),
        ("pkg/test/__init__.py", "from .._fakes import FakeClient\n"),
    ])?;

    assert!(
        public.contains(&"pkg/_fakes.py:FakeClient".to_owned()),
        "{public:?}"
    );
    Ok(())
}

#[test]
fn should_publish_the_items_of_a_module_a_public_module_binds_under_its_own_name()
-> Result<(), String> {
    let relative = public_items(&[
        ("pkg/__init__.py", "from . import _impl as impl\n"),
        (
            "pkg/_impl.py",
            "class Widget:\n    pass\n\n\ndef _hidden() -> int:\n    return 1\n",
        ),
    ])?;
    let absolute = public_items(&[
        ("pkg/__init__.py", "import pkg._impl as impl\n"),
        ("pkg/_impl.py", "class Widget:\n    pass\n"),
    ])?;
    let kept_private = public_items(&[
        ("pkg/__init__.py", "from . import _impl\n"),
        ("pkg/_impl.py", "class Widget:\n    pass\n"),
        (
            "pkg/quiet.py",
            "from . import _impl2 as impl2\n\n__all__ = []\n",
        ),
        ("pkg/_impl2.py", "class Gadget:\n    pass\n"),
    ])?;

    assert_eq!(relative, ["pkg/_impl.py:Widget"]);
    assert_eq!(absolute, ["pkg/_impl.py:Widget"]);
    assert_eq!(kept_private, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_read_every_module_that_an_absolute_import_could_name() -> Result<(), String> {
    let public = public_items(&[
        ("app/api.py", "from pkg._impl import One, Two\n"),
        ("src/a/pkg/__init__.py", ""),
        ("src/a/pkg/_impl.py", "class One:\n    pass\n"),
        ("src/b/pkg/__init__.py", ""),
        ("src/b/pkg/_impl.py", "class Two:\n    pass\n"),
    ])?;

    assert_eq!(public, ["src/a/pkg/_impl.py:One", "src/b/pkg/_impl.py:Two"]);
    Ok(())
}

#[test]
fn should_keep_every_non_underscore_name_public_for_each_unreadable_all() -> Result<(), String> {
    let public = public_items(&[
        (
            "starred.py",
            "__all__ = [\"x\", *other.__all__]\n\n\ndef x() -> int:\n    return 1\n\n\ndef extra() -> int:\n    return 2\n",
        ),
        (
            "sliced.py",
            "__all__ = [\"y\"]\n__all__[:] = [\"z\"]\n\n\ndef y() -> int:\n    return 1\n\n\ndef z() -> int:\n    return 2\n",
        ),
        (
            "looked_up.py",
            "__all__ = [\"w\"]\nglobals()[\"__all__\"] = []\n\n\ndef w() -> int:\n    return 1\n\n\ndef v() -> int:\n    return 2\n",
        ),
    ])?;

    assert_eq!(
        public,
        [
            "looked_up.py:v",
            "looked_up.py:w",
            "sliced.py:y",
            "sliced.py:z",
            "starred.py:extra",
            "starred.py:x"
        ]
    );
    Ok(())
}

#[test]
fn should_mark_a_source_and_its_stub_together() -> Result<(), String> {
    let public = public_items(&[
        ("pkg/__init__.py", "from ._impl import Widget\n"),
        ("pkg/__init__.pyi", "from ._impl import Widget as Widget\n"),
        ("pkg/_impl.py", "class Widget:\n    pass\n"),
        ("pkg/_impl.pyi", "class Widget: ...\n"),
    ])?;

    assert_eq!(
        public,
        [
            "pkg/__init__.py:Widget",
            "pkg/_impl.py:Widget",
            "pkg/_impl.pyi:Widget"
        ]
    );
    Ok(())
}

#[test]
fn should_publish_a_module_alias_inside_a_namespace_package() -> Result<(), String> {
    let namespace = public_items(&[
        ("ns/api.py", "from . import _impl as impl\n"),
        ("ns/_impl.py", "class Widget:\n    pass\n"),
    ])?;
    let root_level = public_items(&[
        ("api.py", "from . import _impl as impl\n"),
        ("_impl.py", "class Gadget:\n    pass\n"),
    ])?;

    assert_eq!(namespace, ["ns/_impl.py:Widget"]);
    assert_eq!(root_level, ["_impl.py:Gadget"]);
    Ok(())
}

#[test]
fn should_publish_the_module_an_import_as_binds_even_under_the_package_name() -> Result<(), String>
{
    let public = public_items(&[
        ("lib/__init__.py", "import lib._impl as lib\n"),
        ("lib/_impl.py", "class Widget:\n    pass\n"),
    ])?;

    assert_eq!(public, ["lib/_impl.py:Widget"]);
    Ok(())
}

#[test]
fn should_keep_a_package_public_when_it_replaces_itself_or_sets_a_hook_by_call()
-> Result<(), String> {
    for init in [
        "import sys\n\nsys.modules[__name__] = Lazy(__name__)\n",
        "import sys\n\nsys.modules[__name__].__class__ = Lazy\n",
        "globals()[\"__getattr__\"] = hook\n",
        "setattr(this, \"__getattr__\", hook)\n",
    ] {
        let public = public_items(&[
            ("pkg/__init__.py", init),
            ("pkg/_heavy.py", "class Heavy:\n    pass\n"),
        ])?;

        assert_eq!(public, ["pkg/_heavy.py:Heavy"], "{init}");
    }
    Ok(())
}

#[test]
fn should_keep_everything_public_below_a_lazy_root_package() -> Result<(), String> {
    let by_hook = public_items(&[
        (
            "__init__.py",
            "def __getattr__(name: str) -> object:\n    raise AttributeError(name)\n",
        ),
        ("_heavy.py", "class Heavy:\n    pass\n"),
        ("_sub/__init__.py", ""),
        ("_sub/core.py", "class Core:\n    pass\n"),
    ])?;
    let by_replacement = public_items(&[
        (
            "__init__.py",
            "import sys\n\nsys.modules[__name__] = Lazy(__name__)\n",
        ),
        ("_heavy.py", "class Heavy:\n    pass\n"),
    ])?;

    assert_eq!(
        by_hook,
        [
            "__init__.py:__getattr__",
            "_heavy.py:Heavy",
            "_sub/core.py:Core"
        ]
    );
    assert_eq!(by_replacement, ["_heavy.py:Heavy"]);
    Ok(())
}

#[test]
fn should_count_the_folders_below_a_root_package_that_is_not_lazy() -> Result<(), String> {
    let public = public_items(&[
        ("__init__.py", ""),
        ("_sub/core.py", "class Core:\n    pass\n"),
        ("open/core.py", "class Open:\n    pass\n"),
    ])?;

    assert_eq!(public, ["open/core.py:Open"]);
    Ok(())
}
