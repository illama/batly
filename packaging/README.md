# Paquets : winget, Scoop, Chocolatey

Ce dossier contient les **modèles** de manifestes des gestionnaires de paquets Windows, et
`appsettings.toml` (réglages par défaut livrés dans le zip portable).

| Dossier | Gestionnaire | Fichier distribué |
|---|---|---|
| `winget/` | winget (Microsoft) | `Batly-Setup-<v>.exe`, installation par utilisateur, `/S` |
| `scoop/` | Scoop | `Batly-<v>-portable.zip` (réglages conservés via `persist`) |
| `chocolatey/` | Chocolatey | `Batly-Setup-<v>.exe`, `/S` |

Les modèles contiennent des marqueurs (`{{VERSION}}`, `{{SETUP_SHA256}}`, `{{ZIP_SHA256}}`,
`{{RELEASE_DATE}}`) : **ne les soumettez pas tels quels**.

## Générer les manifestes d'une version

Automatiquement : le workflow `.github/workflows/release.yml` (déclenché par un tag `v*`)
produit l'artefact `packages-<version>` avec les manifestes remplis.

À la main, après `./build.sh` (qui remplit `dist/`) :

```sh
packaging/update-manifests.sh 4.1.0            # lit dist/, écrit dist/packages/
packaging/update-manifests.sh 4.1.0 chemin/vers/dist chemin/de/sortie
```

Les sommes SHA-256 sont lues dans `dist/SHA256SUMS.txt` (et recalculées pour vérification si
les fichiers sont présents). Elles doivent correspondre **exactement** aux fichiers publiés
dans la release GitHub : générez les manifestes à partir des fichiers de la release finale
(signés, le cas échéant), jamais d'une compilation locale différente.

Résultat :

```
dist/packages/
├── winget/manifests/i/illama/Batly/<v>/   4 fichiers .yaml
├── scoop/batly.json
└── chocolatey/batly.nuspec + tools/chocolateyinstall.ps1, chocolateyuninstall.ps1
```

> Attendez que la release GitHub soit publiée (le workflow la publie directement) : les trois
> gestionnaires téléchargent les fichiers depuis ses URL publiques.

## winget

Identifiant : `illama.Batly`. Manifestes au format 1.6 (version, installer, locale par
défaut `en-US`, locale `fr-FR`).

**Première soumission** (une PR sur [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs)) :

1. Forker `microsoft/winget-pkgs` sur GitHub.
2. Copier `dist/packages/winget/manifests/i/illama/Batly/<v>/` au même chemin dans le fork.
3. Vérifier localement sous Windows :
   ```powershell
   winget validate --manifest manifests\i\illama\Batly\<v>
   winget settings --enable LocalManifestFiles     # une fois, en administrateur
   winget install --manifest manifests\i\illama\Batly\<v>
   ```
4. Ouvrir la PR (titre conseillé : `New package: illama.Batly version <v>`), signer le CLA
   Microsoft au premier passage du bot. Les robots de validation installent le paquet dans
   une machine virtuelle ; un modérateur fusionne ensuite (quelques jours).

**Avec wingetcreate** (plus simple, fait le fork et la PR pour vous) :

```powershell
winget install Microsoft.WingetCreate
wingetcreate submit --token <PAT GitHub> dist\packages\winget\manifests\i\illama\Batly\<v>
# versions suivantes, sans les modèles :
wingetcreate update illama.Batly --version <v> --urls https://github.com/illama/batly/releases/download/v<v>/Batly-Setup-<v>.exe --submit --token <PAT>
```

Le jeton GitHub (PAT « classic » avec la portée `public_repo`) n'est nécessaire que pour
`--submit`.

Remarques : SmartScreen et les antivirus des robots winget sont plus tolérants avec un
exécutable signé (voir `SIGNING.md`) ; l'installateur relance Batly à la fin, c'est voulu.

## Scoop

Le manifeste `scoop/batly.json` installe la **version portable** : `appsettings.toml` est
conservé entre les mises à jour (`persist`). Dans ce mode la mise à jour intégrée à Batly
ne remplace rien (elle ouvre seulement la page de la version) : on met à jour avec
`scoop update batly`.

`checkver` / `autoupdate` suivent les releases GitHub : une fois le manifeste dans un bucket,
les mises à jour se font seules (Excavator dans un bucket perso, ou les robots d'Extras).

**Bucket personnel** (immédiat) :

1. Créer un dépôt public, par ex. `illama/scoop-bucket`, avec un dossier `bucket/`
   (le modèle [ScoopInstaller/BucketTemplate](https://github.com/ScoopInstaller/BucketTemplate)
   fournit l'action Excavator qui applique `autoupdate` chaque jour).
2. Y copier `dist/packages/scoop/batly.json` dans `bucket/`.
3. Les utilisateurs :
   ```powershell
   scoop bucket add illama https://github.com/illama/scoop-bucket
   scoop install illama/batly
   ```

**Bucket Extras** (plus visible) : PR sur
[ScoopInstaller/Extras](https://github.com/ScoopInstaller/Extras) ajoutant `bucket/batly.json`,
en suivant leur `CONTRIBUTING.md` (le projet doit être un minimum connu : étoiles, ancienneté).
Tester avant : `scoop install .\batly.json`, puis
`.\bin\checkver.ps1 batly -u` dans le dépôt du bucket pour vérifier `autoupdate`.

## Chocolatey

Paquet `batly` qui télécharge l'installateur depuis GitHub et le lance avec `/S` (somme
SHA-256 vérifiée par Chocolatey). La désinstallation retire le programme, le raccourci, le
démarrage automatique et l'entrée « Applications installées », mais garde les réglages
(`%APPDATA%\Batly`).

> L'installation est **par utilisateur** : comme `choco` s'exécute généralement en
> administrateur, Batly est installé pour le compte qui lance la commande.

1. Créer un compte sur [community.chocolatey.org](https://community.chocolatey.org/account/Register),
   puis récupérer sa clé d'API dans *Account → API Keys*.
2. Sous Windows, avec Chocolatey installé :
   ```powershell
   cd dist\packages\chocolatey
   choco pack                                   # produit batly.<v>.nupkg
   choco install batly --source . -y          # test local (en administrateur)
   choco uninstall batly -y
   choco apikey --key <CLÉ> --source https://push.chocolatey.org/
   choco push batly.<v>.nupkg --source https://push.chocolatey.org/
   ```
3. Le paquet passe par la modération automatique (validation, vérification, VirusTotal) puis
   humaine pour la première version (de quelques jours à quelques semaines). Les versions
   suivantes approuvées vite si rien ne change dans les scripts.

Pour automatiser plus tard : [chocolatey-au](https://github.com/chocolatey-community/chocolatey-au)
ou un `choco push` dans le workflow de release avec un secret `CHOCOLATEY_API_KEY`.
