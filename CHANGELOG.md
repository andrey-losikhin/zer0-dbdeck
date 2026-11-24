# Changelog

Notable user-facing changes are recorded here. The project uses
[Semantic Versioning](https://semver.org/); pre-1.0 behavior may change.

## [Unreleased]

### Added

- Initial public source distribution of the 0.1.0 development codebase.
- PostgreSQL/MySQL/MariaDB browsing, SQL sessions and staged editing.
- Encrypted portable connection storage and keyring/gopass integrations.
- Streaming table CSV export, mapped CSV import and native backup/restore.
- Keyboard command palette, shortcut reference and themed native UI.
- Community documentation, contribution/security policies and local validation commands.
- Object search in the connection tree, named table views and a task menu with
  progress and cancellation.
- SQL completion from catalog metadata, local SQL formatting and estimated query
  plans for PostgreSQL, MySQL and MariaDB.
- Related rows by foreign key, table structure comparison and Markdown reports.

### Changed

- Improved layouts for smaller windows, tab management, connection forms and
  filter controls.
- Expanded cell previews and previews of pending table edits. Table browsing
  uses cursor pagination for supported primary-key orders.
- Table CSV exports use distinct suggested filenames for shown rows and all rows.

No release tag or prebuilt package has been published as part of the initial
repository setup. See Git history for subsequent development changes.
