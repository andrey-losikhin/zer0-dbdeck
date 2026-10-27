# Third-party notices

The root Apache-2.0 license covers original DBDeck code. It does not replace
licenses, attribution requirements or trademark rules for third-party material.

| Material | Origin | License / notice |
| --- | --- | --- |
| Tabulator 6.6.1 and retained 6.5.3 assets | [Oli Folkerd / Tabulator](https://github.com/olifolkerd/tabulator) | MIT; [6.6.1 license](../assets/vendor/tabulator-6.6.1/LICENSE), [6.5.3 license](../assets/vendor/tabulator-6.5.3/LICENSE) |
| Database vendor logos | [Simple Icons](https://simpleicons.org/) | [CC0-1.0](../assets/icons/vendors/LICENSE-Simple-Icons.md); vendor trademark rules still apply |
| Database open/closed icons | [Pictogrammers / Material Design Icons](https://pictogrammers.com/library/mdi/) | [Pictogrammers Free License](../assets/icons/vendors/LICENSE-MDI.txt), including Apache-2.0 terms |
| Solar UI icon adaptations | [480 Design](https://www.figma.com/community/file/1166831539721848736), [Solar set](https://icon-sets.iconify.design/solar/) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/); 480 Design attribution, adaptations recolored/resized/converted to symbolic strokes |
| Tokyo Night palette and derived styles | [Tokyo Night](https://github.com/tokyo-night/tokyo-night-vscode-theme) | [MIT](licenses/Tokyo-Night-MIT.txt) |
| Catppuccin palette and derived styles | [Catppuccin](https://github.com/catppuccin/catppuccin) | [MIT](licenses/Catppuccin-MIT.txt) |

Rust dependencies are resolved in `Cargo.lock` and retain their package licenses.
GTK, libadwaita, GtkSourceView, WebKitGTK and their system dependencies retain their
upstream licenses; they are not relicensed by the root LICENSE. Binary distributors
must also meet the applicable dependency and system-library obligations.

Public TLS test certificates are supplied separately by the developer and are
not bundled with this repository or application. The testing guide identifies
upstream fixture revisions. Never use public test keys outside isolated tests.
