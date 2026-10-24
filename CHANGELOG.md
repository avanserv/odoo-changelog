# Changelog

## [0.3.0](https://github.com/avanserv/odoo-changelog/compare/v0.2.0...v0.3.0) (2026-10-24)


### ⚠ BREAKING CHANGES

* **extract:** Pair renames across modules, by shared words and content sketches ([#37](https://github.com/avanserv/odoo-changelog/issues/37))

### Features

* **extract:** Pair renames across modules, by shared words and content sketches ([#37](https://github.com/avanserv/odoo-changelog/issues/37)) ([c25b234](https://github.com/avanserv/odoo-changelog/commit/c25b23446818275fcdd86ad4179508b104ef83bc))
* **extract:** Record the symbols a merge's conflict resolution changed ([#38](https://github.com/avanserv/odoo-changelog/issues/38)) ([a0a7c14](https://github.com/avanserv/odoo-changelog/commit/a0a7c14a27cb2942f1ad1fc4d0ab362f986853bc))
* **upgrade:** Match upgrade events to the renames they confirm and report the detector's recall ([#36](https://github.com/avanserv/odoo-changelog/issues/36)) ([c92c088](https://github.com/avanserv/odoo-changelog/commit/c92c08829c366923202a575b35928fdf0cc3186d))
* **upgrade:** Read upgrade scripts into rename, move and removal events ([#34](https://github.com/avanserv/odoo-changelog/issues/34)) ([7dc87e3](https://github.com/avanserv/odoo-changelog/commit/7dc87e384333e3db34d79389e5a572d69bca9b77))

## [0.2.0](https://github.com/avanserv/odoo-changelog/compare/v0.1.0...v0.2.0) (2026-10-23)


### ⚠ BREAKING CHANGES

* **extract:** Pair renamed functions whose body changed by MinHash similarity ([#32](https://github.com/avanserv/odoo-changelog/issues/32))
* **extract:** Pair access rules re-declared as access rights under one xmlid ([#31](https://github.com/avanserv/odoo-changelog/issues/31))
* **extract:** Record xmlid and template references and list a rename's referrers ([#29](https://github.com/avanserv/odoo-changelog/issues/29))
* **extract:** Pair removed and added symbols as renames ([#26](https://github.com/avanserv/odoo-changelog/issues/26))
* **extract:** Pair symbol moves across files and modules ([#25](https://github.com/avanserv/odoo-changelog/issues/25))

### Features

* **extract:** Classify events as functional or technical and flag breaking ones ([#27](https://github.com/avanserv/odoo-changelog/issues/27)) ([b5a2a41](https://github.com/avanserv/odoo-changelog/commit/b5a2a41749111ae5661e006d5d76e6fd2f4fdf1e))
* **extract:** Extract data files by xmlid and views down to their named nodes ([#15](https://github.com/avanserv/odoo-changelog/issues/15)) ([55a685d](https://github.com/avanserv/odoo-changelog/commit/55a685dd2dce1f910dea23b59efad8e62ea635b1))
* **extract:** Extract JS modules, exports, components, registry entries and patches ([#16](https://github.com/avanserv/odoo-changelog/issues/16)) ([bffd8a7](https://github.com/avanserv/odoo-changelog/commit/bffd8a718dbddf3241d2c204531d34092db30eae))
* **extract:** Extract mail JS models and static/tests helpers ([#21](https://github.com/avanserv/odoo-changelog/issues/21)) ([f2101d3](https://github.com/avanserv/odoo-changelog/commit/f2101d31bd78c80f62a7f10cdc504e4e3cc300d4))
* **extract:** Extract models, fields, methods and routes from Python files ([#13](https://github.com/avanserv/odoo-changelog/issues/13)) ([68f90ff](https://github.com/avanserv/odoo-changelog/commit/68f90ffd711b90962731040803088c6a23122947))
* **extract:** Extract module manifests with their dependencies, data and assets ([#14](https://github.com/avanserv/odoo-changelog/issues/14)) ([b4dac4e](https://github.com/avanserv/odoo-changelog/commit/b4dac4e960b5cb6e32eff301d86fba88959e0582))
* **extract:** Extract module-level Python constants ([#20](https://github.com/avanserv/odoo-changelog/issues/20)) ([8bfcef1](https://github.com/avanserv/odoo-changelog/commit/8bfcef1c1c55e411f794ca5430acaabb4751bf11))
* **extract:** Extract OWL templates and SCSS variables and mixins ([#17](https://github.com/avanserv/odoo-changelog/issues/17)) ([6b6f63c](https://github.com/avanserv/odoo-changelog/commit/6b6f63c5effd21b43543d6e5b82d5821c028fafc))
* **extract:** Extract records from model-named data CSVs ([#23](https://github.com/avanserv/odoo-changelog/issues/23)) ([d5dee1d](https://github.com/avanserv/odoo-changelog/commit/d5dee1d8ee747894c48bd4d5d65152c3eb3b645f))
* **extract:** Extract SCSS map entries, usages and :root properties ([#22](https://github.com/avanserv/odoo-changelog/issues/22)) ([e875c3f](https://github.com/avanserv/odoo-changelog/commit/e875c3f8f0f7d4347b103d07a290f2bfab21ec4a))
* **extract:** Pair access rules re-declared as access rights under one xmlid ([#31](https://github.com/avanserv/odoo-changelog/issues/31)) ([948a1ae](https://github.com/avanserv/odoo-changelog/commit/948a1aec0b4bb6fd603bb27c74b1505333057e78))
* **extract:** Pair removed and added symbols as renames ([#26](https://github.com/avanserv/odoo-changelog/issues/26)) ([c2b4944](https://github.com/avanserv/odoo-changelog/commit/c2b4944e2843314f11b76626b53a4372dc752b9f))
* **extract:** Pair renamed functions whose body changed by MinHash similarity ([#32](https://github.com/avanserv/odoo-changelog/issues/32)) ([56063b6](https://github.com/avanserv/odoo-changelog/commit/56063b6203c37f693a782619993257e33337b75e))
* **extract:** Pair symbol moves across files and modules ([#25](https://github.com/avanserv/odoo-changelog/issues/25)) ([c28979e](https://github.com/avanserv/odoo-changelog/commit/c28979e0cc63f21b6ae69d65e4e78430d6bc0891))
* **extract:** Record per-commit symbol events through a blob cache ([#12](https://github.com/avanserv/odoo-changelog/issues/12)) ([839f44f](https://github.com/avanserv/odoo-changelog/commit/839f44f0eef273bf0dcac29d657f9fc6fd343c91))
* **extract:** Record xmlid and template references and list a rename's referrers ([#29](https://github.com/avanserv/odoo-changelog/issues/29)) ([7babf11](https://github.com/avanserv/odoo-changelog/commit/7babf1129016072c4eb4251b7c0e1769effd6c2d))


### Bug Fixes

* **extract:** Hash JS bodies with their local names numbered ([#28](https://github.com/avanserv/odoo-changelog/issues/28)) ([7682bbb](https://github.com/avanserv/odoo-changelog/commit/7682bbb3dd7c6f7169d29d89ac2f9d8f5894a3c1))
* **extract:** Merge an xmlid declared bare and module-qualified in one file ([#30](https://github.com/avanserv/odoo-changelog/issues/30)) ([543cf1e](https://github.com/avanserv/odoo-changelog/commit/543cf1ec70a3fefce775eacfec9bb9699e1cf85d))
* **store:** Keep the clone path out of the database ([#9](https://github.com/avanserv/odoo-changelog/issues/9)) ([3b0f769](https://github.com/avanserv/odoo-changelog/commit/3b0f769ac566b48b7e7a8c51849a2c53afc2d03f))
* **walk:** Fail on unknown repos, walk errors and master-claimed commits ([#11](https://github.com/avanserv/odoo-changelog/issues/11)) ([78f5980](https://github.com/avanserv/odoo-changelog/commit/78f59800aab6cf958121f7586259f42172701ed9))
* **walk:** Record the commits a merge brought in parents first ([#10](https://github.com/avanserv/odoo-changelog/issues/10)) ([e578ef8](https://github.com/avanserv/odoo-changelog/commit/e578ef8fdb5910636fdcddfe80d77241ba9534f3))
* **walk:** Refuse to resume when a fork moved since the last walk ([#6](https://github.com/avanserv/odoo-changelog/issues/6)) ([db83a3d](https://github.com/avanserv/odoo-changelog/commit/db83a3d58e2ec43403d503495462e4d08bff4389))
* **walk:** Try the release-file fallback for every unresolved branch of a shallow clone ([#8](https://github.com/avanserv/odoo-changelog/issues/8)) ([805a2e6](https://github.com/avanserv/odoo-changelog/commit/805a2e64db26de511157e9a624194c856b5ed695))

## 0.1.0 (2026-10-19)


### Features

* **walk:** Record Odoo commit history per version in SQLite ([#1](https://github.com/avanserv/odoo-changelog/issues/1)) ([b50cdd7](https://github.com/avanserv/odoo-changelog/commit/b50cdd7a9434bafeba280938ec0592ab656735c8))
