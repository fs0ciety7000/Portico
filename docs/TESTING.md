# Tests de Portico

| Niveau | Où | Commande | En CI |
|---|---|---|---|
| Unitaires Rust (core, thème, TUI, commandes GUI) | `#[cfg(test)]` à côté du code | `cargo test` | oui |
| Interopérabilité OpenSSH (`sshd`, `ssh-agent`, `ssh-keygen`) | `crates/portico-core/tests/openssh_interop.rs` | `cargo test -p portico-core --test openssh_interop -- --ignored` | oui |
| Logique TypeScript de la GUI (raccourcis, liste d'hôtes, arbre de panneaux) | `apps/portico-gui/scripts/test-*.ts` | `pnpm check` | oui |
| **E2E de l'app de bureau** (Tauri sous WebKitGTK, vrai `sshd`) | `apps/portico-gui/e2e/` | `pnpm e2e` | non (voir plus bas) |
| GUI et TUI sous **Windows** (compilation MSVC, tests unitaires, installation NSIS et démarrage) | `ci.yml` (`gui-windows`), `release.yml` (`tui` msvc, `gui-windows`) | runners `windows-latest` | oui |
| **Windows 11, manuel** (les 14 fonctionnalités, ACL, agent, Gestionnaire d'identification, SmartScreen) | ce document, « Plan de test manuel Windows 11 » | sur le PC du propriétaire | non |

Le mode démo (`pnpm dev`, Playwright) sert au design ; il ne remplace pas l'E2E : seul ce dernier passe par
l'IPC Tauri, le moteur réel, WebKitGTK et un serveur SSH.

## E2E de l'app de bureau

`apps/portico-gui/e2e/run.sh` (alias `pnpm e2e`, Linux uniquement) :

1. construit l'app en debug avec le frontend embarqué (`pnpm tauri build --debug --no-bundle`, binaire dans
   `$CARGO_TARGET_DIR/debug/portico-gui`) ; `--no-build` réutilise le binaire existant ;
2. crée un environnement jetable (`e2e/env.sh`) : `HOME` et dossiers XDG à lui (jamais le vrai `~/.ssh`),
   un `sshd` OpenSSH sur `127.0.0.1` (port libre, clé d'amorçage, `SetEnv HOME` vers un HOME distant jetable,
   SFTP démarrant dans ce HOME), un `ssh-agent` (`SSH_AUTH_SOCK`), un petit serveur HTTP pour le tunnel et un
   `~/.ssh/config` à trois hôtes : `alpha` (avec une valeur `SetEnv` qui doit rester masquée), `jumped`
   (`ProxyJump alpha`) et `gamma` (n'accepte que la clé générée pendant le test). Pas de bus de session D-Bus :
   pas de keyring système, donc le **vault chiffré** est le store, comme sur un serveur ou un WM minimal ;
3. lance `Xvfb`, puis `tauri-driver` (qui pilote `WebKitWebDriver`) ;
4. exécute `e2e/gui.e2e.ts` (runner de test de Node, client WebDriver minimal sans dépendance dans
   `e2e/webdriver.ts`).

Le scénario, dans l'ordre : démarrage (hôtes, pas de vault) ; connexion et clé hôte acceptée (`known_hosts`) ;
commande dans le terminal, dont la sortie revient par le PTY (le shell pose un titre OSC que la barre d'état
affiche) ; statut et latence ; enregistrement de la session (confirmation, `REC` à l'événement du moteur, arrêt sans confirmation, fichier `0600` qui contient la sortie, liste, lecture jusqu'au bout puis fermeture) ; création du vault ; génération d'une clé ed25519 avec passphrase ; ajout à
l'agent pour une heure puis retrait (`ssh-add -l`) ; copy-id vers `alpha`, vérification avec la clé (prompt de
passphrase, mémorisée dans le vault) ; connexion à `gamma` avec cette seule clé, sans prompt ; téléchargement
SFTP ; snippet avec placeholder, puis refus du collage multi-ligne sous `sh` (sans bracketed paste) et
exécution à la place ; favori, tags, groupe et notes (`hosts.toml`), la sélection suivant l'hôte déplacé ;
ProxyJump ; panneau de logs et configuration effective (valeur sensible masquée, absente du DOM) ; tunnel
`-L` (affiché avec sa spec, trafic HTTP, arrêt) ; thème mémorisé (`settings.toml`) ; redémarrage : déverrouillage
du vault au démarrage et connexion par clé sans prompt ; hôte Portico (phase 6b) : « Add host… » avec la clé
`bootstrap` choisie parmi les clés découvertes et un mot de passe mémorisé (absent de `hosts.conf`, chiffré dans
le vault, `~/.ssh/config` intact), badge et bandeau `Include`, connexion sans prompt, port modifié puis rétabli,
suppression confirmée ; import Termix : « Import hosts… », export JSON généré par `env.sh` (un hôte vers le `sshd`,
avec une clé autorisée côté serveur et absente de `~/.ssh`) choisi dans le sélecteur natif avec `xdotool`, aperçu
(aucune clé cochée, texte de la clé jamais affiché), clé cochée (`1`), rapport, clé écrite dans
`~/.ssh/portico/` (`0600`, dossier `0700`), `IdentityFile` et `IdentitiesOnly` dans `hosts.conf`, puis connexion
avec cette seule clé, sans prompt.

Une capture est prise à chaque étape clé (et à chaque échec d'attente) dans `<racine>/shots/`. La racine
jetable est supprimée à la fin, sauf avec `E2E_KEEP=1` ou si elle est donnée par `E2E_ROOT=<dossier>`.

### Prérequis (Debian/Ubuntu)

```bash
sudo apt install openssh-server openssh-client xvfb xdotool webkit2gtk-driver libwebkit2gtk-4.1-dev
cargo install tauri-driver --version 2.1.0 --locked
```

Node 22.18+ (types TypeScript retirés nativement) et pnpm. `sshd` tourne sous l'utilisateur courant (le test
se connecte en tant que lui) ; sous root il lui faut `/run/sshd`, que le script crée.

```bash
cd apps/portico-gui
pnpm e2e                                  # build debug + test (~1 min hors build)
E2E_KEEP=1 e2e/run.sh --no-build          # garder captures et logs (sshd.log, driver.log)
e2e/run.sh --serve --no-build             # environnement prêt, WebDriver sur un port aléatoire, pour piloter à la main
```

> **Attention, `--serve`** : `tauri-driver` lance tout programme qu'une requête WebDriver lui désigne, et une
> page web peut l'atteindre par une requête cross-origin « simple » (sans preflight). Les ports sont donc
> aléatoires par défaut (`E2E_DRIVER_PORT` et `E2E_NATIVE_PORT` pour les fixer ; `4444` refusé en `--serve`
> sauf `E2E_I_KNOW=1`). Pendant un run ou un `--serve`, ne naviguez pas, et jamais sur une machine partagée.

### Limites

- **Pas en CI** : il faut WebKitGTK, son driver WebDriver, `tauri-driver`, Xvfb et un `sshd` ; à lancer avant une
  release et après toute modification notable de la GUI.
- **Linux seulement** : `tauri-driver` ne pilote pas WKWebView (macOS).
- Le sélecteur de fichiers natif est un dialogue GTK hors WebDriver : l'import le pilote avec `xdotool`
  (`Ctrl-L`, chemin, Entrée, d'où `xdotool` dans les prérequis) ; l'upload SFTP et le dossier de téléchargement
  ont été vérifiés à la main de la même façon, pas dans le test.
- Le contenu du terminal est rendu par xterm.js (WebGL ou canvas), illisible par WebDriver : le test observe ses
  effets (titre OSC, fichiers distants) et les captures montrent le rendu.
- Le presse-papiers (copie des logs) se vérifie à la main sous Xvfb (`xsel -b -o`).

## Plan de test manuel Windows 11 (propriétaire)

Critère de sortie de la phase 8 (`docs/PLAN.md`) : **les 14 fonctionnalités du MVP sur Windows 11, dans les
deux UIs**. Aucune machine Windows n'est disponible pour le développement : la CI (`windows-latest`) compile,
lance les tests unitaires, installe et démarre la GUI, mais ne peut rien juger de ce qui suit (agent SSH réel,
Gestionnaire d'identification d'une vraie session, ACL vues par OpenSSH, SmartScreen, rendu, clavier). Ce
plan se déroule sur le PC Windows 11 du propriétaire ; cocher dans une copie et reporter les écarts en issue
(capture, `PORTICO_LOG=debug`, version de Windows `winver`).

### Préparation (une fois)

- [ ] **Binaires** : artefacts `gui-windows-x64` et `tui-x86_64-pc-windows-msvc` d'un essai à blanc du
      workflow Release lancé sur la branche à tester (*Actions → Release → Run workflow*, `distribute` vide ;
      `docs/RELEASE.md`), ou fichiers d'une Release. Vérifier les sommes (`Get-FileHash`).
- [ ] **OpenSSH pour Windows** (client, livré avec Windows 11) et son agent :
      ```powershell
      # PowerShell administrateur, une fois : le service est désactivé par défaut
      Get-Service ssh-agent | Set-Service -StartupType Automatic
      Start-Service ssh-agent
      ```
- [ ] **Un serveur SSH de test** joignable : VM Linux, WSL 2 avec `openssh-server`, ou un hôte de labo ;
      idéalement deux (le second en bastion, pour `ProxyJump`). Une session sous un compte dont le nom
      contient un espace ou un accent (`C:\Users\Zoé Martin`) couvre les chemins délicats : à faire au
      moins une fois.
- [ ] `%USERPROFILE%\.ssh\config` avec : un `Host` simple, un hôte derrière `ProxyJump`, un motif
      (`Host *.lab`), un `Include config.d/*` (chemin **relatif** : il se résout sous `%USERPROFILE%\.ssh`,
      comme OpenSSH pour Windows), un bloc `Match host …`, des `IdentityFile ~/.ssh/…`.
- [ ] **Windows Terminal** (par défaut sous Windows 11) pour la TUI ; un essai rapide dans la console
      historique (`conhost.exe`) suffit.

### Installation, SmartScreen, désinstallation

- [ ] Télécharger l'installeur **avec Edge** (marque « Internet »), le lancer : SmartScreen affiche
      « Windows a protégé votre ordinateur » ; *Informations complémentaires → Exécuter quand même*. **Aucune
      invite UAC** (installation par utilisateur).
- [ ] État de *Smart App Control* noté (*Sécurité Windows → Contrôle des applications et du navigateur*) :
      « Activé » doit bloquer Portico (non signé, attendu, documenté) ; « Évaluation » ou « Désactivé » non.
- [ ] Installé dans `%LOCALAPPDATA%\Portico` ; raccourci du menu Démarrer ; *Paramètres → Applications
      installées* montre « Portico » et sa version.
- [ ] L'app installée se lance sans nouvelle invite SmartScreen ; **première fenêtre en moins de 500 ms**
      (perçu, ou chronomètre de capture d'écran) ; pas de flash blanc avec le thème clair ou sombre.
- [ ] Zip TUI : *Propriétés → Débloquer* (ou `Unblock-File`), extraire, `portico --version` dans Windows
      Terminal ; **interface utilisable en moins de 100 ms** à froid.
- [ ] Désinstaller (*Applications installées*) : `%APPDATA%\Portico\portico\config\` (réglages,
      `hosts.toml`, `hosts.conf`), `%LOCALAPPDATA%\Portico\portico\data\` (vault, enregistrements,
      journaux : sous le dossier d'installation, que le désinstalleur ne vide pas) et `%USERPROFILE%\.ssh`
      **restent** ; avec la case « supprimer les données », seul `%LOCALAPPDATA%\dev.portico.app` (WebView2)
      disparaît.
- [ ] Réinstaller une version **plus ancienne** par-dessus en silencieux (`setup.exe /S`) : refusé
      (`allowDowngrades: false`).

### Les 14 fonctionnalités du MVP (GUI et TUI)

Chaque ligne se fait dans la GUI **et** dans la TUI (raccourcis : `docs/KEYBINDINGS.md`, colonne Linux
pour la GUI sous Windows : `Ctrl`, `Ctrl-Shift` dans un terminal).

1. **Parsing de `~/.ssh/config`**
   - [ ] GUI · [ ] TUI : tous les hôtes du fichier et des `Include` relatifs apparaissent ; un `Match`
     s'applique ; la config effective (panneau debug, onglet *Effective config*) montre l'origine
     `fichier:ligne` avec des chemins `~/.ssh/…`.
   - [ ] Un `Include C:/Users/<moi>/.ssh/autre` (absolu, barres obliques) et un `IdentityFile` en
     `C:\Users\<moi>\.ssh\cle` (barres inverses) sont compris ; `~` vaut `%USERPROFILE%` ; un
     `IdentityFile ~/ma\ cle` ou `'~/ma cle'` (échappement et guillemets simples d'OpenSSH) aussi.
   - [ ] Utilisateur par défaut : sans `User`, Portico se connecte avec le même nom que `ssh.exe`
     (`ssh -G <hôte> | findstr /b user`) : nom du compte en minuscules, `domaine\nom` pour un compte de
     domaine.
2. **Recherche floue, tags, groupes**
   - [ ] GUI · [ ] TUI : recherche floue, `#tag`, `*` (favoris), `@groupe` ; ajout et retrait d'un tag,
     d'un groupe ; `hosts.toml` écrit dans `%APPDATA%\Portico\portico\config\`.
3. **Connexion SSH interactive (PTY)**
   - [ ] GUI · [ ] TUI : connexion, `htop` / `vim` / `tmux`, redimensionnement de la fenêtre, couleurs,
     souris dans `htop`, Unicode (`echo é 漢字 ✓`).
   - [ ] Première connexion : la question de clé hôte ; accepter **écrit** `%USERPROFILE%\.ssh\known_hosts`
     (lisible ensuite par `ssh.exe` d'OpenSSH : `ssh <hôte>` ne repose pas la question).
   - [ ] Clé hôte modifiée (régénérer la clé du serveur) : **refus bloquant**, sans bouton « accepter ».
   - [ ] `ProxyJump` via le bastion ; `ProxyCommand` refusé avec un message clair.
   - [ ] Authentification par mot de passe, par clé chiffrée (passphrase demandée), par l'agent,
     *keyboard-interactive* (si le serveur l'offre).
4. **SFTP : explorateur, upload, download**
   - [ ] GUI · [ ] TUI : navigation, création de dossier, renommage, suppression ; upload par le sélecteur
     de fichiers **natif Windows** et (GUI) par glisser-déposer depuis l'Explorateur ; download dans
     `%USERPROFILE%\Downloads` (dossier par défaut), puis dans un dossier choisi.
   - [ ] Un fichier distant au nom **invalide sous Windows** (`touch 'a:b' 'con' 'x.' 'a\b'` côté serveur)
     n'est **jamais** écrit ailleurs que dans le dossier de téléchargement (refus explicite attendu ; jamais
     de flux NTFS `a:b`, de périphérique `CON`, ni de chemin hors du dossier).
   - [ ] Un fichier de plusieurs centaines de Mio : progression, annulation, reprise propre (`.part`).
5. **Clés locales, ssh-agent, certificats**
   - [ ] GUI · [ ] TUI : les clés de `%USERPROFILE%\.ssh` sont listées (avec un certificat `-cert.pub`
     s'il existe, validité affichée).
   - [ ] **Agent OpenSSH (tube nommé)** : `ssh-add C:\Users\<moi>\.ssh\id_ed25519` dans PowerShell, puis la
     liste de l'agent dans Portico montre la clé ; ajout et retrait depuis Portico visibles par `ssh-add -l`.
   - [ ] `SSH_AUTH_SOCK` défini sur un tube (`$env:SSH_AUTH_SOCK = '\\.\pipe\openssh-ssh-agent'`) : utilisé ;
     `IdentityAgent //./pipe/openssh-ssh-agent` dans `~/.ssh/config` aussi (la graphie `\\.\pipe\…` y perd une
     barre inverse, comme avec `ssh.exe`) ; service `ssh-agent` **arrêté** (état par défaut) : à la
     connexion, rien d'autre qu'une ligne `Debug` « no ssh-agent (service not running) », et dans le panneau
     des clés une erreur claire avec le *hint* (activer et démarrer le service), aucune attente.
   - [ ] `SSH_AUTH_SOCK` sur le socket de Git Bash (`/tmp/ssh-…/agent.N`) : erreur
     `agent_endpoint_unsupported` (« unset it to use the Windows agent »).
   - [ ] **Tube usurpé** : service arrêté, un autre compte local (session ouverte par *Changer d'utilisateur*)
     crée `\\.\pipe\openssh-ssh-agent` (par exemple `npiperelay` ou un petit serveur PowerShell
     `System.IO.Pipes.NamedPipeServerStream`) : l'ajout d'une clé à l'agent dans Portico est **refusé**
     (`agent_untrusted`), rien n'est envoyé ; la connexion passe à la méthode suivante.
   - [ ] Agent bloqué (un serveur de tube qui accepte sans jamais répondre) : la connexion bascule sur le
     mot de passe ou la clé après environ 5 s (« did not answer: skipped » dans le journal), jamais 60 s.
   - [ ] Ajout à l'agent : la boîte de dialogue dit que l'agent Windows garde la clé jusqu'à son retrait
     (y compris après redémarrage) ; une durée (1 h) refusée par l'agent donne un *hint* clair, sans parler
     d'agent verrouillé.
   - [ ] Pageant (PuTTY) lancé seul : non pris en charge, message clair (documenté).
6. **Génération de clés**
   - [ ] GUI · [ ] TUI : ed25519, rsa 3072, ecdsa, avec passphrase ; `ssh.exe -i <clé> <hôte>`
     d'OpenSSH **accepte** la clé privée créée (il refuse une clé lisible par d'autres : c'est le test des
     ACL vu par OpenSSH) ; refus d'écraser un fichier existant.
7. **Copie de clé publique (copy-id)**
   - [ ] GUI · [ ] TUI : vers un serveur Linux ; puis connexion par clé sans mot de passe ; relancer ne
     duplique pas la ligne dans `authorized_keys`.
8. **Port forwarding local et distant**
   - [ ] GUI · [ ] TUI : `-L 8080:localhost:80` puis `curl http://localhost:8080` dans PowerShell ;
     `-R` ; port déjà pris (`python -m http.server 8080`) : erreur et *hint* ; le pare-feu Windows ne doit
     **pas** demander d'autorisation pour un bind `localhost` (une invite signifierait un bind sur toutes les
     interfaces).
9. **Sessions récentes, favoris, onglets multi-sessions**
   - [ ] GUI · [ ] TUI : section Recent, favori `*`, plusieurs onglets et panneaux divisés, fermeture d'un
     onglet ; quitter l'app ferme proprement les sessions (pas de processus `portico-gui.exe` restant dans le
     Gestionnaire des tâches).
10. **Snippets**
    - [ ] GUI · [ ] TUI : création, édition, exécution globale / par hôte / par tag, placeholders ;
      `snippets.toml` dans `%APPDATA%\Portico\portico\config\`.
11. **Statut de connexion et latence**
    - [ ] GUI · [ ] TUI : `connecting → authenticating → connected` avec latence ; coupure réseau (Wi-Fi
      coupé) : passage en erreur avec *hint*, reconnexion.
12. **Thèmes et raccourcis**
    - [ ] GUI · [ ] TUI : Obsidian, Basalt, Travertine Night, Marble, Auto ; **Auto suit Windows**
      (*Paramètres → Personnalisation → Couleurs → mode clair/sombre*) ; GUI : la **barre de titre native**
      prend l'apparence du thème peint (clair/sombre). TUI : Auto lit `COLORFGBG`, sinon sombre (pas de
      requête OSC 11 sous Windows, documenté).
    - [ ] Raccourcis du tableau Linux de `docs/KEYBINDINGS.md` ; palette `Ctrl-K` ; aucun raccourci ne
      déclenche une fonction de Windows (`Win`, `Alt-Tab`, `Alt-F4` restent à Windows).
13. **Secrets : Gestionnaire d'identification (vault en repli)**
    - [ ] GUI · [ ] TUI : mémoriser un mot de passe d'hôte ; il apparaît dans *Panneau de configuration →
      Gestionnaire d'identification → Informations d'identification Windows → génériques* (service
      `dev.portico`), avec la persistance **Ordinateur local** (`cmdkey /list` : « Persistance : Ordinateur
      local », jamais « Entreprise » : pas de copie vers un profil itinérant), **jamais** en clair dans
      `%APPDATA%\Portico\` ni `%LOCALAPPDATA%\Portico\portico\` (`findstr /s /i <motdepasse> …\*` ne trouve
      rien) ni dans les journaux.
    - [ ] Supprimer l'hôte Portico : l'identifiant disparaît du Gestionnaire. Vault chiffré : création,
      déverrouillage, changement de passphrase.
14. **Mode debug : journaux par session**
    - [ ] GUI · [ ] TUI : panneau debug, journaux d'une connexion réussie et d'un échec, « Copy all » ;
      aucun secret, aucun contenu de terminal.

### Vérifications propres à Windows

**ACL des fichiers créés** (équivalent de `0600`/`0700`). Pour chacun, `icacls <chemin>` doit montrer un
DACL **protégé** (aucune entrée héritée `(I)`) qui n'accorde l'accès qu'à l'utilisateur courant (et aux
seuls comptes que l'ADR des ACL retient, `docs/STACK.md`) ; ni `Everyone`, ni `Users`, ni `Authenticated
Users` :
```powershell
icacls "$env:USERPROFILE\.ssh\id_portico_test"          # clé générée par Portico
icacls "$env:USERPROFILE\.ssh\portico"                  # dossier des clés importées, et ses fichiers
icacls "$env:APPDATA\Portico\portico\config\hosts.toml"
icacls "$env:APPDATA\Portico\portico\config\hosts.conf"
icacls "$env:LOCALAPPDATA\Portico\portico\data"         # vault, enregistrements, journaux (local, jamais itinérant)
icacls "$env:USERPROFILE\.ssh\known_hosts"              # après acceptation d'une clé hôte
```
- [ ] Le **propriétaire** de chacun est l'utilisateur (`(Get-Acl <chemin>).Owner`), y compris quand Portico a
      été lancé **en administrateur** (sinon Windows y mettrait `BUILTIN\Administrators`).
- [ ] Une clé privée rendue lisible par d'autres (`icacls <clé> /grant Users:R`) : Portico avertit (ligne
      `Warn` du journal de connexion, avec la commande `icacls` exacte : chemin absolu et nom du compte,
      sans `%VAR%`, à coller tel quel dans PowerShell), comme OpenSSH (`ssh -i` répond « UNPROTECTED
      PRIVATE KEY FILE »).
- [ ] `known_hosts` **système** : un compte standard crée `C:\ProgramData\ssh\ssh_known_hosts` (possible
      tant qu'OpenSSH Server n'a pas créé le dossier) avec la clé d'un faux serveur : Portico l'**ignore**
      (ligne `Warn` « skipped (not owned by the system…) ») et pose la question de clé hôte. Le même
      fichier créé par un administrateur (propriétaire `Administrators`, non modifiable par les
      utilisateurs) est utilisé. Lancer Portico avec `$env:PROGRAMDATA = 'C:\Users\<moi>\x'` ne change pas
      le dossier lu (dossier connu, pas la variable).
- [ ] Sous Windows, un fichier `known_hosts` **global** doit appartenir à la machine (`SYSTEM`,
      `Administrators` ou `TrustedInstaller`) et n'être modifiable par aucun autre compte : un
      `GlobalKnownHostsFile` qui appartient à l'utilisateur (par exemple `GlobalKnownHostsFile
      ~/global_kh`) est **ignoré** avec une ligne `Warn` « skipped (not owned by the system…) », pour la
      connexion comme pour les mots de passe retenus (liés aux clés de ces seuls fichiers de confiance).
- [ ] **Points d'analyse** : un *junction* (`mklink /J`, sans droits admin) ou un lien symbolique
      (`mklink`, mode développeur) à la place de `hosts.toml`, du dossier `data` ou d'une destination de
      download : refusé, jamais suivi.
- [ ] **`~/.ssh` en junction** (décision : suivi, comme un `~/.ssh` lien symbolique sous Unix) : déplacer
      `%USERPROFILE%\.ssh` vers `D:\ssh` et `mklink /J %USERPROFILE%\.ssh D:\ssh` : les hôtes, `known_hosts` et
      les clés sont lus, la génération et l'import de clés fonctionnent si `D:\ssh` vous appartient et
      qu'aucun autre compte ne peut y écrire (sinon `key_unsafe_dir`, avec la commande `icacls` exacte) ;
      `~/.ssh/portico` en junction reste refusé.
- [ ] **Volume sans ACL** (clé USB **FAT32** puis **exFAT**) : `~/.ssh` en junction vers un dossier de la
      clé (le dossier des enregistrements n'est pas réglable : il ne peut pas être testé là) : génération
      et import de clés fonctionnent, une ligne `Warn` « no ACL support on this volume » est journalisée
      (comme les modes Unix sur un tel montage), aucune erreur `InvalidData` ; `ssh.exe` d'OpenSSH, lui,
      peut refuser ces clés (attendu) ; un `GlobalKnownHostsFile` posé sur ce volume est ignoré (« no file
      permissions to verify »).
- [ ] **Aucun droit administrateur** n'est demandé, à aucun moment (installation, premier lancement,
      génération de clé, tunnel).
- [ ] **Durcissement du processus** : ce que le rapport `PORTICO_LOG=debug` annonce au démarrage
      (`hardening` : `core_dumps` appliqué = rapport WER sans le tas, `ptrace` non disponible) correspond à
      l'ADR Windows ; un plantage provoqué laisse au plus un rapport WER **sans tas**
      (`%LOCALAPPDATA%\Microsoft\Windows\WER`) ; un *dump* complet n'existe que si un administrateur a
      configuré `LocalDumps` (`%LOCALAPPDATA%\CrashDumps`, hors de portée de Portico, documenté).
- [ ] `portico.exe` et `portico-gui.exe` copiés dans `Téléchargements` à côté d'une DLL système factice
      (`version.dll`) : la DLL n'est pas chargée (`/DEPENDENTLOADFLAG:0x800` ; vérifiable aussi par
      `llvm-readobj --coff-load-config`).

**WebView2 et fenêtre (GUI)**
- [ ] Hors terminal, `F5`, `Ctrl-R`, `Ctrl-Shift-R`, `Alt-←`, `Ctrl-P`, `Ctrl-F`, `F7` **ne font rien**
      (pas de rechargement : les sessions restent ouvertes ; pas d'impression, de barre de recherche ni de
      navigation arrière). Dans un terminal, ces touches **atteignent le serveur** (`cat -v` puis `F5` affiche
      `^[[15~`, `Ctrl-R` lance la recherche de bash).
- [ ] Clic droit hors champ de texte : pas de menu « Actualiser / Enregistrer sous / Imprimer » ; dans un
      champ : couper / copier / coller ; dans un terminal : le menu de Portico.
- [ ] **Clavier AZERTY** (ou autre disposition avec `AltGr`) dans un terminal : `AltGr-0` `@`, `AltGr-3`
      `#`, `AltGr-4` `{`, `AltGr-5` `[`, `AltGr-6` `|`, `AltGr-8` `\`, `AltGr-E` `€` arrivent tels quels
      (Windows envoie `AltGr` comme `Ctrl-Alt` : aucun raccourci `Ctrl-Alt-…` de Portico ne doit les voler).
- [ ] Copier / coller (`Ctrl-Shift-C` / `Ctrl-Shift-V` dans un terminal, presse-papiers Windows `Win-V`).
- [ ] Liens du terminal : la confirmation puis l'ouverture dans le **navigateur par défaut** (y compris un
      lien avec `&` et `%` dans la requête, ouvert à l'identique).
- [ ] Mise à l'échelle 125 % / 150 % et deux écrans de densités différentes (déplacer la fenêtre) :
      texte net, terminal recalé ; fenêtre maximisée, ancrée (`Win-←`), restaurée.
- [ ] Le dossier d'enregistrements (« Recordings ») s'affiche en `~/…` et s'ouvre ; un enregistrement se
      relit.

**TUI dans Windows Terminal**
- [ ] `Ctrl-Space` (préfixe) arrive à Portico ; `Ctrl-Space` deux fois l'envoie au serveur.
- [ ] OSC 52 : copier la ligne `Include` (`y`) remplit le presse-papiers Windows.
- [ ] Souris (clic, molette) dans l'explorateur et dans `htop` ; redimensionnement ; 80×24 lisible.
- [ ] `conhost.exe` : démarrage et connexion possibles (rendu moins fidèle accepté).

**Distribution**
- [ ] Scoop : `scoop install .\portico.json` (manifeste rendu, `docs/RELEASE.md`) puis `portico --version`,
      `scoop uninstall portico` ; une fois le bucket créé : `scoop bucket add portico …` et
      `scoop install portico/portico`.
- [ ] winget : `winget validate --manifest <dossier>` sans erreur pour les deux paquets ;
      `winget install --manifest <dossier>` (après `winget settings --enable LocalManifestFiles`) installe
      la GUI (par utilisateur) et la commande `portico` ; `winget uninstall` les retire.
- [ ] La vérification des mises à jour (si activée) propose la bonne commande selon l'installation (Scoop,
      winget, installeur) ; à défaut, le lien vers la Release.
