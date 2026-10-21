# SCSS extractor

The `scss` extractor reads every `.scss` file under `static/` outside test directories, vendored
Bootstrap in `static/lib` included, since themes override its `!default` variables. Names are
global, so every qualname ends with `@<addon>`, the addon that declares the symbol.

| Kind | Declared by | Example |
| --- | --- | --- |
| `scss_variable` | a `$name:` assignment at global scope | `$spacer@theme_bistro` |
| `scss_mixin`, `scss_function` | `@mixin`, `@function` at global scope | `o-to-rem@web` |
| `scss_placeholder` | a `%name { }` rule at global scope | `%o-x@web` |

A variable keeps its rendered `value`, `default` and `global` flags, the control-flow headers it
sits in as `within` (a theme's `@if` assigns globals), and later assignments in the file as
`reassigned`. Mixins and functions keep their `params` and a `body` hash. Values and bodies ignore
layout, comments and trailing commas. A broken stylesheet keeps the statements around the error
and leaves a warning.
