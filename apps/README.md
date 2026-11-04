# Revive app store

The Revive launcher can install apps that are **only a JSON manifest**: the UI
fetches a URL and shows the reply as a list or as value cards. No code from the
store ever runs on the device.

The default catalogue is [`apps/index.json`](index.json) in this repository,
served from
`https://raw.githubusercontent.com/siriani/revive-phone/main/apps/index.json`.
A device can point at another catalogue with `indice=<URL>` in
`/etc/revive/loja.conf`.

## Catalogue format

```json
{
  "versao": 1,
  "apps": [
    {
      "id": "cripto",
      "nome": "Cripto",
      "descricao": "Short description shown in the store.",
      "autor": "Revive",
      "icone": "★",
      "cor": "#7a4b12",
      "categoria": "Finanças",
      "manifesto": "https://raw.githubusercontent.com/siriani/revive-phone/main/ui-slint/apps/cripto.json",
      "sha256": "8b218fe363bdc1cdb3c6f05a02667a9acda1c828d82cf34f5f43187001a239e0",
      "requer": []
    }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `versao` | Catalogue format version. Currently `1`. |
| `apps[].id` | Unique identifier, used as `/etc/revive/apps/<id>.json`. |
| `apps[].nome` | Tile label. |
| `apps[].descricao` | One or two sentences for the store screen. |
| `apps[].autor` | Who maintains the manifest. |
| `apps[].icone` | One character, e.g. `★ ☁ ✉ ⚙ ⌂ ◷ ♫ ≋ ✱`. |
| `apps[].cor` | Tile colour `#rrggbb`. Use a dark colour so white text is readable. |
| `apps[].categoria` | Free-form group name shown in the store. |
| `apps[].manifesto` | Raw URL of the manifest JSON. Must be HTTPS. |
| `apps[].sha256` | SHA-256 of the manifest file, lowercase hex. Checked before installing. |
| `apps[].requer` | Human-readable settings the user must fill in, e.g. `"config: server URL"`. |

The manifest itself is documented in
[`ui-slint/apps/README.md`](../ui-slint/apps/README.md).

## Publishing an app

1. Write the manifest and add it to `ui-slint/apps/<name>.json`.
2. Compute its hash and copy it into the catalogue entry:

   ```sh
   shasum -a 256 ui-slint/apps/<name>.json
   ```

3. Add an entry to `apps/index.json` with the raw URL of the manifest
   (`https://raw.githubusercontent.com/siriani/revive-phone/main/ui-slint/apps/<name>.json`)
   and the hash from step 2.
4. Open a pull request. After it is merged the app shows up for every device
   that uses the default catalogue.

If the manifest changes, the hash in the catalogue must change in the same
commit, otherwise installs fail with a checksum error (nothing is written to
the device).

## Installing and removing

```sh
# from the device, after picking the app in the store
cat /etc/revive/apps/<name>.json
sudo rc-service revive-ui restart
```

Removal deletes `/etc/revive/apps/<id>.json`. The catalogue is cached in
`/var/cache/revive/loja.json`, so the store still lists apps while offline.
