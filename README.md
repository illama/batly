# illama_logitechbatteries — LGSTray (Rust)

[![CI](https://github.com/illama/illama_logitechbatteries/actions/workflows/ci.yml/badge.svg)](https://github.com/illama/illama_logitechbatteries/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/illama/illama_logitechbatteries)](https://github.com/illama/illama_logitechbatteries/releases/latest)
[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
![Windows 10/11 x64](https://img.shields.io/badge/Windows-10%20%7C%2011%20x64-0078D6)

Niveau de batterie des périphériques Logitech dans la zone de notification de Windows.

**[⬇ Télécharger la dernière version](https://github.com/illama/illama_logitechbatteries/releases/latest)**

Réécriture complète en Rust de [LGSTrayBattery](https://github.com/andyvorld/LGSTrayBattery)
(C# / .NET 8 / WPF). Même principe, mêmes réglages, même API HTTP — dans un seul
exécutable natif d'environ 1 Mo, sans runtime .NET, sans `hidapi.dll`, sans processus auxiliaire.

> **État** : le code compile pour Windows x64 et la logique portable (protocole HID++,
> réglages, serveur HTTP, G HUB, rendu des icônes) est couverte par des tests automatisés
> avec un périphérique simulé. La couche Win32 (icône de notification, accès HID,
> installateur) n'a **pas encore été exécutée sur un vrai Windows ni avec du vrai matériel**.
> En cas de souci, lancez `lgstray.exe --debug` et joignez `lgstray.log`.

## Installation

| Fichier | Usage |
|---|---|
| `LGSTray-Setup-4.0.0.exe` | Installateur par utilisateur (aucun droit administrateur). `/S` pour une installation silencieuse. |
| `LGSTray-4.0.0-portable.zip` | Version portable : décompresser et lancer `lgstray.exe` (les réglages restent dans le dossier). |

L'installateur copie l'application dans `%LOCALAPPDATA%\Programs\LGSTray`, crée un raccourci
dans le menu Démarrer, propose le lancement au démarrage de Windows et enregistre la
désinstallation dans « Applications installées » (`lgstray.exe --uninstall`).

Windows 10 (1607) ou plus récent, x64.

## Utilisation

Clic (gauche ou droit) sur l'icône :

- cocher les périphériques à afficher — une icône par périphérique coché ;
- **Afficher le niveau en chiffres** : pourcentage au lieu de la jauge ;
- **Notifications de batterie faible** (seuil réglable, 15 % par défaut) ;
- **Lancer au démarrage de Windows** ;
- **Actualiser maintenant**, ouvrir le fichier de réglages, ouvrir la page web.

L'icône suit le type de périphérique (souris, clavier, casque), le thème clair/sombre de la
barre des tâches, l'échelle d'affichage (DPI) et l'état de charge. L'interface est en français
ou en anglais selon la langue de Windows.

## Ce qui change par rapport à l'original

| | LGSTrayBattery 3.0.3 (C#) | LGSTray 4 (Rust) |
|---|---|---|
| Distribution | zip avec .NET 8 embarqué, ou .NET 8 à installer | installateur 1,5 Mo / exe ~1 Mo, aucune dépendance |
| Processus | 2 (interface + `LGSTrayHID`), IPC MessagePack | 1 |
| Accès HID | `hidapi.dll` via P/Invoke | API Windows directes, E/S asynchrones (overlapped) |
| Attente | lectures avec délai de 100 ms en boucle | entièrement événementiel : aucun réveil périodique |
| Découverte des fonctions | parcours de toute la table HID++ du périphérique | 5 requêtes ciblées |
| Changement d'état (branchement du câble…) | visible au prochain sondage (jusqu'à 10 min) | immédiat (événements batterie HID++) |
| Périphérique en veille | ré-interrogé toutes les 10 s | notifications du récepteur + attente progressive |
| Bluetooth LE / interface « longue » seule | non géré | géré |
| Jauge | 4 paliers | continue, colorée (vert / orange / rouge) |
| Affichage numérique | « 100 » tronqué, pas d'état de charge | taille ajustée, couleur selon l'état |
| Batterie faible | — | notification Windows |
| Thème | surveillance WMI | message système, sans WMI |
| Explorer redémarré | — | icônes recréées automatiquement |
| Sortie de veille du PC | — | relecture automatique |
| Serveur HTTP | HTTP.sys (droits admin pour `0.0.0.0`), pas d'échappement XML | sockets standards, XML échappé, **+ JSON** |
| Identifiant de secours | `GetHashCode()` (change à chaque lancement) | hachage stable |
| Réglages | `appsettings.toml` + `user.config` .NET | un seul `appsettings.toml`, écriture atomique |
| Installation | — | installateur + désinstallateur |

## Réglages

`%APPDATA%\LGSTray\appsettings.toml` (ou à côté de l'exécutable en mode portable).
Le fichier de l'original est accepté tel quel ; redémarrer LGSTray après une modification manuelle.

```toml
[UI]
numericDisplay = false
selectedDevices = []
lowBatteryThreshold = 15     # 0 = jamais
language = "auto"            # "auto", "en", "fr"

[HTTPServer]
enabled = true
addr = "localhost"           # ou une adresse IP, ou "0.0.0.0"
port = 12321
useIpv6 = true

[GHub]
enabled = true               # lecture via Logitech G HUB s'il tourne

[Native]
enabled = true               # dialogue direct HID++
retryTime = 10               # secondes avant nouvel essai après un échec
pollPeriod = 600             # secondes entre deux lectures réussies
disabledDevices = []         # ex. ["G403", "G502"]
```

## API HTTP

| Route | Réponse |
|---|---|
| `GET /` ou `/devices` | page HTML listant les périphériques |
| `GET /device/{id ou nom}` | XML, mêmes balises que l'original (+ `status`, `online`, `source`) |
| `GET /api/devices` | tableau JSON |
| `GET /api/device/{id ou nom}` | objet JSON |

```xml
<xml><device_id>…</device_id><device_name>G Pro Wireless</device_name><device_type>Mouse</device_type>
<battery_percent>69.00</battery_percent><battery_voltage>3.92</battery_voltage><mileage>-1.00</mileage>
<charging>False</charging><last_update>2026-11-21T09:04:05Z</last_update>
<status>discharging</status><online>True</online><source>native</source></xml>
```

Différence volontaire : `last_update` est en ISO 8601 UTC (l'original dépendait de la langue du système).

## Compilation

```sh
# Linux / macOS / WSL : rustup + zig + cargo-zigbuild
rustup target add x86_64-pc-windows-gnu
./build.sh            # tests, puis dist/LGSTray-Setup-<v>.exe et dist/LGSTray-<v>-portable.zip
```

Sous Windows (toolchain MSVC) : `cargo build --release -p lgstray`, puis
`set LGSTRAY_PAYLOAD=<chemin de lgstray.exe>` et `cargo build --release -p lgstray-setup`.

`cargo test -p lgstray` fonctionne sur n'importe quel système.

## Organisation du code

```
app/src/
  hidpp.rs      moteur HID++ 1.0/2.0, indépendant du système (testé avec un périphérique simulé)
  native.rs     Windows : énumération HID, E/S overlapped, un fil par récepteur
  ghub.rs       client WebSocket de Logitech G HUB
  http.rs       serveur HTTP (std::net)
  icon.rs       rendu des icônes (RGBA)
  tray.rs       icônes de notification, menu, boucle de messages
  config.rs     réglages TOML
  model.rs      registre des périphériques partagé
  uninstall.rs  désinstallation
setup/          installateur (embarque lgstray.exe)
build/          script de ressources partagé (icône, version, manifeste)
```

## Licence et remerciements

GPL-3.0, comme le projet d'origine d'[andyvorld](https://github.com/andyvorld/LGSTrayBattery),
dont proviennent le fonctionnement général, la table tension → pourcentage et les pictogrammes.
Protocole HID++ d'après [Solaar](https://github.com/pwr-Solaar/Solaar). Pictogrammes : The Noun Project
(souris : projecthayat ; clavier : HideMaru ; casque : Peter Lakenbrink).
