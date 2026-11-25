Batly 5.5.0 : pilotez vos périphériques depuis Home Assistant.

## Nouveau
- **Contrôles Home Assistant (MQTT)** : en plus des capteurs (batterie, charge, tension, autonomie), les périphériques Logitech de ce PC apparaissent avec leurs commandes, selon ce que chacun gère :
  - **éclairage RGB** comme une lumière : allumer, éteindre, couleur, luminosité ;
  - **sensibilité (DPI)**, **taux de rapport**, **rétroéclairage**, **molette** (cran, libre, intelligente) et **canal Easy-Switch**.
  L'état réel est relu sur l'appareil après chaque commande. Commandes : `<baseTopic>/<id>/set/<réglage>` ; état : `<baseTopic>/<id>/controls` et `<baseTopic>/<id>/light`.
- **Page web et fenêtres** : le texte n'est plus sélectionnable (sauf dans le champ de recherche), comme dans une application.

## Mise en route
1. Home Assistant : module « Mosquitto broker » et intégration MQTT.
2. Batly : Réglages → Domotique, activer MQTT, adresse du Home Assistant, port 1883, utilisateur et mot de passe ; laisser la découverte activée.

## Téléchargements
| Fichier | Usage |
|---|---|
| `Batly-Setup-5.5.0.exe` | Installateur (recommandé) |
| `Batly-5.5.0-portable.zip` | Version portable |
| `SHA256SUMS.txt` | Sommes de contrôle |
| `SHA256SUMS.txt.minisig` | Signature des sommes (clé : `packaging/batly-minisign.pub`) |

Windows 10 (1607) ou plus récent, x64. Les installations 5.x proposeront la mise à jour automatiquement.
