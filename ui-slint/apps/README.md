# Manifest apps

A manifest app is a JSON file that becomes a tile in the Revive launcher. The UI
fetches a URL (HTTP GET, JSON response) and shows the result as a list or as
value cards. No recompilation is needed.

## Where the files go

Copy the files to `/etc/revive/apps/<name>.json` on the device, then restart the
UI so it reloads them:

```sh
sudo cp apps/cripto.json /etc/revive/apps/
sudo rc-service revive-ui restart
```

Files are read in alphabetical order. A file with invalid JSON, or without `id`
or `fonte.url`, is skipped and an error is printed to the log.

## Field reference

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | yes | Unique identifier, no spaces. |
| `nome` | no | Tile label (defaults to `id`). |
| `icone` | no | One character: `★ ☁ ✉ ⚙ ⌂ ◷ ♫ ≋ ✱` (defaults to `★`). |
| `cor` | no | Background colour `#rrggbb`. Use a dark colour so white text is readable. |
| `tipo` | no | `lista` (default) or `valores`. |
| `fonte.url` | yes | URL that returns JSON (HTTP GET). |
| `fonte.headers` | no | Extra request headers, e.g. `{"Authorization": "Bearer ..."}`. |
| `itens` | lista | Dot path to the array inside the response. `""` means the response itself. Numeric parts index arrays. If the path points to an object, its values are the items. |
| `titulo` | lista | Dot path inside each item, used as the item title. |
| `sub` | lista | Dot path inside each item, used as the subtitle. |
| `verde_se` | no | `{"campo": "<dot path>", "igual": <JSON value>}`. Items where the field equals the value are shown in green. |
| `valores` | valores | List of `{"rotulo": "Label", "caminho": "dot.path"}`, one card each. |

Paths use dots and indexes, for example `data.monitors`, `0.state` or `attributes.unit_of_measurement`.

## Examples

- `cripto.json`: list of top coins in BRL from the public CoinGecko API (no token).
- `home-assistant.json`: value cards for one Home Assistant sensor via the REST API (needs a long-lived token).
- `uptime-kuma.json`: list of monitors from a public Uptime Kuma status page (`publicGroupList.0.monitorList`, first group only).

## Secrets

Tokens in these files are secrets. Set `chmod 600` on any file that contains
one, and never commit real tokens. Replace `SEU_TOKEN` and the placeholder hosts
only on the device, not in the repository copy.
