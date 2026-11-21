**LGSTray devient Batly** : toutes vos batteries, d'un coup d'œil.

L'application a bien grandi depuis ses débuts (Logitech, mais aussi Razer, Corsair, SteelSeries, manettes Xbox / PlayStation, écouteurs Bluetooth) : elle prend une identité à elle, avec un nouveau nom et une nouvelle mascotte.

## Ce qui change
- **Nouveau nom et nouveau logo** partout : application, installateur, menu, panneau, page web, notifications, 6 langues.
- **Nouveaux emplacements** : `%LOCALAPPDATA%\Programs\Batly` (programme) et `%APPDATA%\Batly` (réglages, historique, journal). L'exécutable s'appelle `batly.exe`.
- **Identifiants techniques renommés** : topic MQTT par défaut `batly`, entités Home Assistant `batly_…`, métriques Prometheus `batly_…`, champ `"source": "batly"` des webhooks, plugin Stream Deck `com.illama.batly`, skin Rainmeter `Batly`.
- Nouveau dépôt : https://github.com/illama/batly (les anciennes adresses redirigent).

## ⚠️ Si vous utilisiez LGSTray
Batly est une nouvelle application : elle **ne reprend pas** les réglages ni l'historique de LGSTray 4.x, et LGSTray ne se met **pas** à jour vers Batly.
1. Désinstallez LGSTray (Paramètres Windows → Applications), sinon les deux tourneront côte à côte.
2. Installez Batly.
3. Si vous utilisez MQTT / Home Assistant, Prometheus, des webhooks, Stream Deck ou Rainmeter, mettez à jour vos automatisations et réinstallez les intégrations depuis le dossier `integrations/`.

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.0.0.exe` | Installateur (recommandé) |
| `Batly-5.0.0-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |

Windows 10 (1607) ou plus récent, x64.
