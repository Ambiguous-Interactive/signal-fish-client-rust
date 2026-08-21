# Brand & Font Attribution

The Signal Fish Client SDK uses the approved Vector design system supplied by
Ambiguous Interactive. The compact mark, favicon, and client-specific banner
are adapted from the design package attached to
[client issue #80](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/80)
and the completed
[Signal Fish Server implementation](https://github.com/Ambiguous-Interactive/signal-fish-server/commit/3d3944f89739d367ddb60f90fbea64352c834a28).
The client repository and published documentation use the same source-vector
identity.

Exact source URLs, dimensions, formats, checksums, and adaptation notes live in
[`assets/PROVENANCE.toml`](assets/PROVENANCE.toml).
The repository's [MIT license](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/LICENSE)
covers the SDK source.

## Bundled fonts

The documentation self-hosts three Latin variable-font subsets so typography
does not depend on a third-party request at runtime:

| Use | Family | License notice |
| --- | --- | --- |
| Display and headings | Space Grotesk | [SIL Open Font License 1.1](assets/fonts/Space-Grotesk-OFL.txt) |
| Body and interface | Hanken Grotesk | [SIL Open Font License 1.1](assets/fonts/Hanken-Grotesk-OFL.txt) |
| Code and data | JetBrains Mono | [SIL Open Font License 1.1](assets/fonts/JetBrains-Mono-OFL.txt) |

Each notice includes the font project's copyright statement and must remain
beside redistributed font files.
