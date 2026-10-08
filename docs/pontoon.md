# Fluent catalog migration

PhotoCraft has generated Fluent catalogs for all 13 current UI languages and a
Pontoon path configuration in `l10n.toml`. The app reads Fluent for plain,
contextual, command-id, and plural lookups. Existing code that translates a
parameterized template before formatting it still reads the TSV catalog for
that template. Do not connect a translation service for writes until that
compatibility path is migrated and the catalogs have a synchronization workflow.

The stable message IDs are in `crates/ui-egui/src/i18n/keys.tsv`. Run
`cargo xtask i18n check` to parse each catalog, check variables, and audit
literal UI calls. `cargo xtask i18n generate --force` converts the TSV catalogs
again; it overwrites Fluent translations and is only for the migration phase.

The remaining migration must preserve all 13 languages, current UI behavior,
and regional terminology: `zh-CN` is Mainland Simplified Chinese, `zh-TW` is
Taiwan Traditional Chinese, and `pt-BR` is Brazilian Portuguese. Current
preferences continue using `zh-hans`, `zh-hant`, and `pt-br` internally.
