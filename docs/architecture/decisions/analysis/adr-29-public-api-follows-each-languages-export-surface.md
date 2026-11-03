# ADR-29: Public API follows each language's real export surface

📌

An item is public API only when code outside the repository can really import it. TypeScript reads the package's `exports` map and follows re-exports from its entry files. Python follows its naming conventions: a leading underscore and `__all__`. When unlace cannot tell, the item stays public.

- Status: `Accepted`
- Date: `2026-11-03`
- Supersedes: ADR-25 in part (its deferral of TypeScript and Python public API, and the testOnly section's sentence that those adapters mark every exported item public)

## 🎯 Motivation

On a TypeScript package whose `package.json` says `"exports": { ".": "./src/index.ts" }`, unlace reported 20 items used only by tests with the wording "It is public, so code outside this repo may use it too; check before moving or deleting it". None of the 20 can be imported from outside: `src/index.ts` never re-exports them, and Node refuses deep imports into a package that declares `exports`. The warning sent readers to look for outside users that cannot exist, and hid the useful message: the item is implemented, tested, and never called.

## 🧭 Context

`Node.public_api` is set by each language adapter. Two consumers read it. Visibility advice never narrows a public item (ADR-25 rule 8). The testOnly finding picks its wording from it: a public item gets the "check before moving or deleting it" wording, any other item gets "move it under test-only code or delete it".

The Rust adapter already asks rust-analyzer for each library crate's real public paths. The TypeScript and Python adapters marked every exported (TypeScript) or non-underscore (Python) item public, and ADR-25 deferred anything more exact.

A Node package that declares `exports` allows imports only through the subpaths it lists. A package without `exports` lets callers import any file (`pkg/lib/foo.js`). Python has no such wall: any module can be imported by its dotted path. A leading underscore on a name or a module, and a module's `__all__`, are the conventions Python uses to say "not part of the public surface".

## ✅ Decision

Each adapter decides public API from the surface its language really exposes, and keeps "public" whenever it cannot be sure.

**TypeScript and JavaScript**

1. Each `package.json` in the analysed tree defines a package. Its `exports` field, in string, subpath-map, conditions-object or array form, lists the entry targets.
2. Each target is mapped back to a source file in the tree. A source path (`./src/index.ts`) is used as is. A build-output path (`./lib/x.js`, `./dist/*.d.ts`) is mapped through the package's `tsconfig.json` `outDir` and `rootDir`, and `.js`, `.mjs`, `.cjs` or `.d.ts` is swapped for the matching source extension. A `*` subpath pattern expands over the files it matches.
3. Public API is every item reachable from an entry file, followed transitively through `export * from` and `export * as ns from`, `export { x } from`, re-exported imports, `export default`, `export =`, `export import x = require()`, and a literal `import()` or `require()` inside a default export. Only export statements are followed; a value that reaches outside code through an object literal or a class `extends` is not traced. Any export form the adapter cannot follow keeps the package's exports public.
4. Every export of the package stays public in each of these cases:
   - the package has no `exports` field (deep imports work);
   - an entry target cannot be mapped to a source file;
   - an entry file is missing from the analysed tree.

**Python**

5. A module-level item is public unless one of these holds:
   - its name starts with `_`;
   - a module or package on its import path, counted from the top package down, starts with `_`;
   - its module defines a plain `__all__` list and the name is not in it.

   Dunder names and modules (`__version__`, `__about__.py`, `__main__.py`) never count as underscore-private. An `__all__` that is built or changed in a way unlace cannot read excludes nothing.
6. A name that a public module imports, and so makes importable from it, is public, along with the original it points to; a module the public module imports under its own name (`from . import _impl as impl`) has its items made public the same way. Here "public module" means rule 5 doesn't make the module private, and the module can be an `__init__.py` or any other module. A package `__init__.py` that sets up lazy loading, either by defining or importing a module-level `__getattr__` or by replacing itself in `sys.modules`, keeps every item in that package public.

**Rust**

7. Unchanged (ADR-25 rule 8).

## 🔀 Alternatives considered

**Treat `main` and `module` as closed entries.** Rejected. Without `exports`, Node still resolves deep imports, so real public API would be called private.

**Python: only what the top-level `__init__.py` exposes.** Rejected. Python cannot block deep imports, so public modules reached by dotted path would be called private.

**Python: count only re-exports made by `__init__.py`.** Rejected. `from pkg.api import Widget` works when `pkg/api.py` imports `Widget` from a private module, so limiting re-exports to `__init__.py` would call an importable item private.

**Keep "every export is public".** Rejected. It produces the misleading wording and the over-suppressed visibility advice described above.

## 🤝 Accepted Trade Offs

- A package without `exports` gets no improvement. This buys never calling a deep-importable item private.
- A build-output mapping that `tsconfig.json` does not explain falls back to "every export public". This buys silence over a wrong verdict.
- Python: a name made public by plain assignment (`Widget = _impl.Widget`) is not traced back to its original, just as TypeScript value flow is not. This buys a rule that follows import statements only.
- A file that unlace does not analyse (ignored by `.gitignore` or `adapters.exclude`, behind a symlinked folder, or in a language it cannot read) can still re-export a closed package's internals without unlace seeing it. This buys closed packages that stay closed in ordinary monorepos.

## ⚖️ Consequences

Items that cannot be imported from outside get the plain testOnly wording. Visibility advice is unchanged for both languages, because neither adapter states a scope ladder; there `public_api` only selects the testOnly wording. Non-literal `import()`, string registries and `importlib` with computed names remain outside what the adapters can see (the advisory and `advice_exempt` rules of ADR-25 still apply).
