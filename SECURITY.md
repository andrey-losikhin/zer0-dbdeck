# Security policy

## Supported versions

Until a stable release, security fixes target the current `main` branch and the
latest published 0.x release, if any. Older snapshots are unsupported.

## Report privately

Use [GitHub private vulnerability reporting](https://github.com/andrey-losikhin/zer0-dbdeck/security/advisories/new)
when available. Alternatively, email
[andrey.losikhin@gmail.com](mailto:andrey.losikhin@gmail.com) with the subject
`zer0-dbdeck security report`. Do not open a public issue for a suspected vulnerability.

Include the affected commit/version, engine and server version, prerequisites,
impact and a minimal synthetic reproducer. Never send live credentials, private
keys, decrypted connection stores, production dumps or personal customer data.

Reports are reviewed when the maintainer is available; no response-time SLA or
bug-bounty program is promised. Confirmed issues will be coordinated privately
and disclosed with remediation information.

## Security boundaries

- Application read-only mode and production confirmations supplement database roles.
  Arbitrary stored functions, server extensions and privileged roles are not sandboxed.
- The `prod` label does not enforce TLS. The transport selector is authoritative.
  TLS without verification does not authenticate the server; disabled TLS does not encrypt it.
- A local attacker who controls the running user, unlock provider, native client,
  process memory or clipboard is outside the encrypted-at-rest storage boundary.
- Native restore executes trusted SQL and native-client commands. An imported file
  can address other databases or cause external effects with the supplied privileges.
- SQL/CSV/database dumps are plaintext; encrypted connection exports are a separate format.
- Do not assume a failed connection means a write or COMMIT did not reach the server.
  DBDeck does not automatically retry uncertain writes.
- Query limits bound the preview, not server CPU, locks or total network traffic.

The project does not claim a formal security audit or universal production certification.
