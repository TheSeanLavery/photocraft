# Fluent catalog staging

PhotoCraft has generated Fluent catalogs for all 13 current UI languages and a
Pontoon path configuration in `l10n.toml`. The app currently reads the TSV
catalogs, so changes made to Fluent files do **not** appear in the UI yet.
Do not connect a translation service for writes to this branch until the runtime
switch and a catalog synchronization workflow are complete.

The stable message IDs are in `crates/ui-egui/src/i18n/keys.tsv`. Run
`cargo xtask i18n check` to parse each catalog, check variables, and audit
literal UI calls. `cargo xtask i18n generate --force` converts the TSV catalogs
again; it overwrites Fluent translations and is only for the staging phase.

The runtime migration must preserve all 13 languages, current UI behavior,
and regional terminology: `zh-CN` is Mainland Simplified Chinese, `zh-TW` is
Taiwan Traditional Chinese, and `pt-BR` is Brazilian Portuguese. Current
preferences continue using `zh-hans`, `zh-hant`, and `pt-br` internally.
