# SCSS extractor

The `scss` extractor reads every `.scss` and `.css` file under `static/` outside test
directories, vendored Bootstrap in `static/lib` included, since themes override its `!default`
variables. Names are global, so every qualname but a stylesheet's ends with `@<addon>`, the addon
that declares the symbol. Sass reads `_` and `-` in a name as one, so variable, mixin and function
names, and the mixins a file includes, are written with `_` folded to `-`: `$o_brand_primary` is
`$o-brand-primary`.

| Kind | Declared by | Example |
| --- | --- | --- |
| `scss_variable` | a `$name:` assignment at global scope | `$spacer@theme_bistro` |
| `scss_map_entry` | a literal top-level key of a global map literal, or of the literal maps a `map-merge` merges | `$o-theme-color-palettes#avantgarde-1@theme_avantgarde` |
| `scss_mixin`, `scss_function` | `@mixin`, `@function` at global scope | `o-to-rem@web` |
| `scss_placeholder` | a `%name { }` rule at global scope | `%o-x@web` |
| `css_property` | a custom property in a global rule whose selector is exactly `:root` | `--o-webclient-color-scheme@web` |
| `scss_stylesheet` | every file, by its path as manifests list it | `web/static/src/webclient/webclient.scss` |

## Attributes

A variable keeps its rendered `value`, `default` and `global` flags, the control-flow headers it
sits in as `within` (a theme's `@if` assigns globals), and later assignments in the file as
`reassigned`. A map entry and a custom property keep their `value`, `within` and `reassigned`
alike, and a map entry its map's `default` flag, so a theme merging one palette into a map changes
that entry only. Mixins and functions keep their `params` and a `body` hash. A stylesheet lists
its `imports` in order, and the mixins it `includes` and the selectors it `extends` anywhere in
the file, sorted. Values and bodies ignore layout, comments and trailing commas. A broken
stylesheet keeps the statements around the error and leaves a warning.
