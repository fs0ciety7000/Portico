# Synchronisation chiffrée par dossier (phase 11)

> **État : S1a livré (fondations pures dans `portico-core/src/sync/`) ; S1b (intégration au moteur) et S1c (UIs) à venir ; S2 et S3 ensuite.**
> Décisions du propriétaire du 2026-10-27 intégrées (§11), revue `agent-security` du même jour appliquée (§12, elle prime sur les
> sections précédentes en cas d'écart). ADR : `docs/STACK.md`, 2026-10-27. Plan : `docs/PLAN.md`, phase 11.

---

## 0. Résumé

- **Transport** : un dossier choisi par l'utilisateur (Dropbox, iCloud Drive, OneDrive, Syncthing, Google Drive, un dépôt git
  qu'il commite lui-même…). Portico y crée `portico-sync/` et n'y fait que des E/S de fichiers locales. **Aucun code réseau** :
  la règle §11 de `CLAUDE.md` (« pas de service tiers sans opt-in et ADR ») est respectée, la sync est opt-in par appareil.
- **Disposition** : **un instantané scellé par appareil** (`devices/<id-aléatoire>.snap`, un seul écrivain par fichier) et un
  `bundle.toml` rarement écrit (paramètres KDF, emplacement de clé, roster scellé, MAC). Le fournisseur n'a jamais deux
  écrivains à fusionner. Les noms de fichiers ne révèlent ni alias ni nom d'hôte.
- **Crypto** : passphrase → Argon2id (bornes propres à la sync) → HKDF → KEK, qui enveloppe une clé de données (DK) aléatoire de
  256 bits par époque. Des sous-clés HKDF de la DK servent au scellement des instantanés (XChaCha20-Poly1305, nonce aléatoire de
  24 octets), au MAC d'en-tête (HMAC-SHA256), au roster, au réplica local, à la comparaison et à une seconde couche AEAD par
  secret (S2). L'AAD lie l'identifiant du bundle, l'époque, la génération du jeu de clés, l'appareil, la séquence de
  publication et les versions de format. Le clair est complété (*padding*) à une puissance de deux.
- **Fusion** : par état. Chaque champ synchronisé est un **registre multi-valeurs à vecteurs de versions pointés** (compteurs par
  appareil, jamais l'horloge murale). Des écritures concurrentes de valeurs différentes deviennent des **frères** montrés comme
  conflits, jamais résolus en silence ; des valeurs égales fusionnent ; une suppression est une **pierre tombale**, donc
  « édition contre suppression » est aussi un conflit visible.
- **Retour arrière / rejeu** : chaque appareil garde, dans son état local scellé, la plus haute séquence et l'empreinte vues par
  appareil et pour `bundle.toml`. Plus bas = retour arrière ; même séquence, autre contenu = fourche. Un attaquant du dossier
  peut **geler** ou **retenir**, mais ni injecter, ni modifier, ni réordonner, ni revenir en arrière sans être détecté par un
  appareil qui a déjà synchronisé.
- **Contenu** : synchronisés : hôtes de `hosts.conf`, métadonnées de `hosts.toml`, snippets, une liste blanche de réglages et
  (S2, opt-in au niveau du bundle) les mots de passe **liés** re-chiffrés par secret. Jamais : clés privées, passphrases de clés,
  `known_hosts`, `~/.ssh/config`, enregistrements, logs, sessions récentes, provenance d'import, consentement à la vérification
  des mises à jour, état local de sync.
- **Livraison** : S1 (tout sauf mots de passe, rotation et révocation) découpé en S1a / S1b / S1c ; S2 = rotation des clés,
  « oublier un appareil », puis mots de passe ; S3 = clés par appareil, signatures, options.

---

## 1. Modèle de menace

### 1.1 Actifs
| Actif | Sensibilité |
|---|---|
| Mots de passe mémorisés | **Haute** : confidentialité et intégrité |
| Hôtes (alias, HostName, User, Port, IdentityFile, ProxyJump) | Moyenne en confidentialité (carte du réseau) ; **haute en intégrité** : un HostName modifié redirige `ssh prod` |
| Métadonnées, snippets | Moyenne. Les snippets sont tapés dans des PTY distants : leur intégrité compte |
| Réglages | Faible |
| DK, KEK, passphrase | Critique |

### 1.2 Adversaires
| # | Adversaire / événement | Capacité | Réponse | Risque résiduel |
|---|---|---|---|---|
| A1 | Fournisseur cloud, co-propriétaire d'un dossier partagé, malware limité au dossier | Lit tout, pour toujours (attaque hors ligne) | Tout est scellé ; Argon2id avec un plancher élevé ; tailles complétées ; noms aléatoires | Voit : nombre d'appareils, moments d'écriture, classe de taille, usage de Portico. Passphrase faible → force brute (§1.3) |
| A2 | Idem, en écriture | Modifie, échange, tronque, crée des fichiers | AEAD par instantané, AAD liée (bundle, époque, appareil, séquence) ; MAC sur `bundle.toml` ; tout ce qui n'est pas authentifié est ignoré et signalé | Aucun sans la DK |
| A3 | Idem | Remet une ancienne copie valide, ou rejoue le fichier d'un appareil sous le nom d'un autre | L'AAD lie l'appareil (échange refusé) ; marque haute locale par appareil → `sync_rollback` ; vecteurs « vus » | Un appareil qui rejoint pour la première fois ne détecte pas un retour arrière cohérent de tout le dossier |
| A4 | Idem | Supprime ou retient (gel) | Un instantané manquant **ne supprime rien** (fusion par état, pierres tombales explicites) ; retenue visible via les vecteurs « vus » et l'ancienneté | Un gel total ressemble à « les autres appareils sont éteints » |
| A5 | **Autre appareil compromis** (malware avec la DK) | Écrit des instantanés valides, lit tous les secrets synchronisés | Changements de destination reçus **retenus jusqu'à confirmation sur cet appareil** (F1) ; snippets modifiés confirmés au premier lancement (F8) ; `known_hosts` jamais synchronisé ; « oublier l'appareil » + rotation (S2) | Un appareil compromis détient tous les mots de passe synchronisés |
| A6 | Appareil perdu ou volé | Trousseau local, éventuellement la DK | DK dans le trousseau système (ou le vault), jamais dans un fichier ; « oublier » + « rotation » depuis un autre appareil (S2) | Jusqu'à la rotation, le voleur lit le dossier s'il l'obtient |
| A7 | Passphrase faible | — | Argon2id ≥ 128 Mio (256 Mio par défaut), jauge, minimum **imposé par le cœur**, générateur diceware | L'UI le dit |
| E1 | Deux appareils éditent hors ligne | — | Frères du registre → liste de conflits | Aucun (rien n'est perdu) |
| E2 | Sync partielle (fichiers livrés un par un, fichier tronqué) | — | Instantanés autonomes et atomiques ; un fichier tronqué échoue à l'AEAD et compte comme « en attente » tant qu'il n'est pas stable sur deux passes | Retard temporaire |
| E3 | Décalage d'horloge | — | L'horloge murale ne sert **jamais** à ordonner ni à collecter, seulement à l'affichage | Heure affichée fausse |
| E4 | Profil cloné (`%APPDATA%` itinérant, image disque, restauration Time Machine) | — | État de sync dans le dossier de données **local** (`%LOCALAPPDATA%`), DK en persistance `Local` ; même id + même séquence + autre empreinte → `sync_fork` ; fichier propre en avance → `sync_restored` → nouvel id | — |

### 1.3 Force brute (ordre de grandeur)
Argon2id m=256 Mio, t=3 : environ 1 s d'un cœur et 256 Mio par essai. Quatre mots diceware (≈51 bits) demandent ~10^15 essais,
hors de portée ; un mot de passe « humain » de 10 caractères (30 à 35 bits) est à la portée d'un attaquant déterminé. D'où le
minimum imposé (§6).

### 1.4 Hors périmètre
Malware sur un appareil **déverrouillé** ; cacher l'usage de Portico ; partage multi-utilisateur avec permissions (un bundle =
un utilisateur ; un dossier partagé entre collègues signifie une DK partagée, et l'UI le dit).

---

## 2. Cryptographie

### 2.1 Primitives
| Usage | Primitive | Crate |
|---|---|---|
| KDF de passphrase | Argon2id v0x13 | `argon2` 0.6 |
| Sous-clés | HKDF-SHA256 | `hkdf` 0.13 + `sha2` 0.11 (directes depuis S1a) |
| AEAD | XChaCha20-Poly1305, nonce aléatoire de 24 octets | `chacha20poly1305` 0.11 |
| MAC | HMAC-SHA256 | `hmac` 0.13 |
| Comparaison en temps constant | — | `subtle` 2.6 (directe depuis S1a) |
| Appareils et signatures (S3) | X25519, Ed25519 | `curve25519-dalek` / `ed25519-dalek` (déjà dans le lock) |
| Aléa | générateur du système (`rand::rngs::SysRng`) | comme `store.rs` et `vault.rs` |

### 2.2 Hiérarchie des clés
```
passphrase ──Argon2id(sel 16 o, m, t, p)──► ikm (32 o)
           ──HKDF-SHA256(sel = bundle_id, info = "portico-sync/1 kek")──► KEK
KEK ──XChaCha20-Poly1305 (AAD de l'emplacement)──► enveloppe DK_époque (32 o aléatoires)
DK ──HKDF-SHA256(sel = bundle_id ‖ époque_be32, info = "portico-sync/1 <nom>")──►
    snapshot (K_snap) | secret (K_secret) | header (K_mac) | compare (K_cmp) | roster (K_roster) | local (K_local)
```
Chaque sous-clé a un seul usage (F14). La passphrase n'enveloppe que la DK :
- **Changer la passphrase** = ré-envelopper la même DK sous un nouveau sel et un nouvel emplacement (génération + 1) ; aucun
  instantané n'est re-chiffré. **Cela ne protège pas d'une passphrase déjà fuitée** : l'historique de versions du fournisseur
  garde l'ancien `bundle.toml`, qui s'ouvre toujours avec l'ancienne passphrase et donne la même DK (F2, testé :
  `old_bundle_copy_opens_with_old_passphrase_after_change`). En S1, le seul remède est « abandonner ce bundle et en créer un
  nouveau » ; la **rotation** (nouvelle époque, nouvelle DK) arrive en S2, avant tout mot de passe synchronisé.
- **Réinitialiser la passphrase** depuis un appareil qui détient la DK, sans l'ancienne : permis derrière une confirmation
  supplémentaire, avec une **alerte bloquante** à acquitter sur tous les autres appareils (décision #8 affinée).
- **Mémoriser la DK** (par défaut) : dans le `SecretStore` (`SecretKey::SyncDataKey`, compte `sync-key:<bundle>:<époque>`,
  persistance Windows `Local`, élément macOS non synchronisable, vault en repli), jamais dans un fichier ; option « ne pas
  mémoriser » pour les machines partagées. « Désactiver la sync » oublie la DK par défaut (F14).

### 2.3 Paramètres KDF (F13, prime sur la première version)
- Nouveaux bundles : **m = 256 Mio, t = 3, p = 1** (≈1 s, hors chemin chaud).
- Bornes **propres à la sync**, vérifiées **avant toute allocation** : plancher 128 Mio / t=3 / p=1, plafond **512 Mio / t=6 /
  p=4**. Jamais `vault::KdfBounds::PRODUCTION`.
- Mémoire Argon2 réservée par `try_reserve_exact` puis `hash_password_into_with_memory` : un manque de mémoire est une erreur
  (`sync_kdf_memory`), jamais un abandon ; elle est remise à zéro après usage.
- Sel de 16 octets, renouvelé à chaque changement de passphrase. L'appareil retient l'empreinte du bundle vérifié (marque
  haute) : des paramètres ou un sel modifiés sans génération ni révision supérieures apparaissent comme une divergence (même
  triplet, autre empreinte) et ne remplacent pas silencieusement le bundle connu.

### 2.4 AEAD, AAD et nonces
- Nonce aléatoire de 24 octets par scellement : rien à persister, aucune réutilisation possible après un crash.
- AAD = encodage canonique (domaine, étiquette, puis champs préfixés par leur longueur u64) :
  - **emplacement** : `keyslot`, bundle_id, époque, **keyset_generation** (F4), type et id d'emplacement, algorithme et version
    KDF, m, t, p, sel. Modifier un de ces champs fait échouer le déballage ;
  - **instantané / réplica** : `snapshot` ou `local`, bundle_id, époque, keyset_generation, appareil, séquence, version
    d'enveloppe, version du corps, longueur complétée. Les mêmes valeurs sont répétées dans le clair et comparées après
    déchiffrement ;
  - **roster** : `roster`, bundle_id, époque, keyset_generation, révision ;
  - **secret** (S2) : `secret`, bundle_id, époque, clé canonique de l'enregistrement, point (appareil, compteur).
- Mauvaise passphrase → `sync_wrong_passphrase`, **indiscernable** d'une altération d'un champ lié à l'emplacement (pas
  d'oracle). MAC ou roster invalide **après** un déballage réussi → `sync_bundle_tampered`.

### 2.5 Métadonnées authentifiées (`bundle.toml`)
`mac = HMAC-SHA256(K_mac, canonique(format, version, bundle_id, époque, keyset_generation, révision, kdf, chaque emplacement,
nonce et chiffré du roster))`, comparé en temps constant. Un appareil qui détient la DK le vérifie à chaque passe ; un nouvel
appareil après le déballage. Aucune écriture dans le dossier ne dépend d'un déballage avant que le MAC soit vérifié (F14).

### 2.6 Remplissage
Clair JSON complété par des espaces à la puissance de deux suivante (instantanés et réplica : 16 Kio minimum, 4 Mio maximum ;
roster : 1 Kio minimum). Secrets internes (S2) complétés à 64 octets (≤ 1 Kio). Pas de compression.

### 2.7 Retour arrière et rejeu
Chaque instantané porte `seq` (strictement croissante), `clock` (compteur de points de l'appareil) et `seen` (séquences
fusionnées par appareil). Localement, **dans le réplica scellé** (F5), l'appareil garde par appareil (seq max, SHA-256 du
fichier, heure locale d'observation) et pour le bundle (époque, keyset_generation, révision, SHA-256).

| Observation | Verdict | Action |
|---|---|---|
| fichier valide avec `seq` < marque | retour arrière | ignoré, `sync_rollback` (nomme l'appareil) |
| même `seq`, autre empreinte | fourche | ignoré, `sync_fork` ; si c'est cet appareil : rejoindre sous un nouvel id |
| `bundle.toml` avec (époque, génération, révision) inférieur | retour arrière (F4) | refusé, `sync_rollback` |
| même triplet, autre contenu | réécritures concurrentes | choix déterministe du gagnant parmi les copies (S1b) |
| format plus ancien qu'un format déjà vu | rétrogradation | refusé (cliquet de version) |
| un `seen[d]` > la `seq` présente pour d | retenu ou pas encore synchronisé | « en attente », avertissement après 24 h |
| **mon propre fichier en avance sur mon état local** | état local restauré (F5) | rejoindre sous un **nouvel id** avant toute écriture |
| mon propre fichier en retard | perdu ou remis en arrière | republier depuis l'état local |
| retour arrière cohérent de tout le dossier vu par un appareil qui n'a jamais synchronisé ; gel total | non détectable | limite documentée |

S3 (optionnel) : signature Ed25519 par appareil, pour l'attribution entre appareils.

### 2.8 Mots de passe au repos (S2)
Inchangé localement (trousseau ou vault). Dans l'instantané : deux couches (AEAD interne sous K_secret liée au compte et au point,
puis AEAD de l'instantané) ; fusion et comparaison ne déchiffrent jamais. Détection de changement : compteur de génération par
compte dans `IndexedSecretStore` (lecture seulement si l'index a changé) et `HMAC(K_cmp, compte ‖ secret)` stocké dans
l'enregistrement scellé pour fusionner des valeurs égales sans déchiffrer (F11). Les DTO de conflit n'ont jamais de valeur.

---

## 3. Format sur disque

### 3.1 Options comparées
| Option | Copies en conflit du fournisseur | Sync partielle | Fuites | Verdict |
|---|---|---|---|---|
| Un seul fichier | Fréquentes à chaque paire d'éditions hors ligne ; certains fournisseurs écrasent | tout ou rien | minimales | rejeté |
| Un fichier par enregistrement | Rares mais par enregistrement ; centaines de fichiers | vues incohérentes | nombre et rythme | rejeté |
| Journal par appareil | aucune | préfixes ; ajouts non atomiques chez la plupart | nombre d'opérations | rejeté en v1 (compaction) |
| **Instantané complet par appareil** | aucune, sauf fourche (détectée) | chaque fichier est autonome | nombre d'appareils, moments, classe de taille | **retenu** |

Chaque instantané contient l'état **fusionné** connu de l'appareil : les données survivent à la perte de l'appareil qui les a
créées, et un nouvel appareil n'a besoin que d'un instantané complet.

### 3.2 Arborescence
```
<dossier choisi>/
└── portico-sync/
    ├── README.txt          # texte clair : « Données Portico chiffrées. Ne pas modifier. »
    ├── bundle.toml         # en-tête, KDF, emplacements, roster scellé, MAC
    └── devices/
        ├── 3f9c0a…e1.snap  # 32 hex = id aléatoire de 128 bits ; un écrivain par fichier
        └── b71d55…09.snap
```
- Écritures : `devices/~<id>.snap.tmp` (`create_new`, `0600` / DACL propriétaire), `fsync`, `rename` ; jamais à travers un lien.
- Lectures plafonnées : instantané ≤ 8 Mio, `bundle.toml` ≤ 64 Kio, `devices/` ≤ 256 entrées, ≤ 8 candidats par appareil et un
  budget d'octets par passe ; au-delà de la limite de listage, la sync se met en pause **sans rien supprimer** (F15).
- Copies en conflit reconnues par préfixe d'id (Dropbox, Syncthing, OneDrive `-MACHINE`, Google Drive / iCloud ` (1)`,
  Nextcloud) : chacune est vérifiée, la plus haute séquence valide gagne. Un appareil ne supprime que les copies **de son propre
  id**, après vérification.
- Placeholders iCloud (`.….snap.icloud`) → « en attente », conseil « Toujours garder sur cet appareil ». OneDrive à la demande
  peut bloquer : lectures sur un thread dédié avec délai (§5.4).
- **Racine sûre** (F9) : racine canonisée à la configuration ; à chaque passe, `symlink_metadata` vérifie que `portico-sync/` et
  `devices/` sont de vrais dossiers (liens refusés ; sous Windows, points d'analyse SYMLINK et MOUNT_POINT refusés, placeholders
  CLOUD_* acceptés) ; refus d'un dossier dans `~/.ssh`, le dossier de config ou de données, revérifié sur le chemin canonique.

### 3.3 `bundle.toml` (format 1, livré en S1a)
```toml
# Portico encrypted sync bundle. Do not edit: any change makes it unreadable.
format = "portico-sync"
version = 1
bundle_id = "base64(16)"
epoch = 1
keyset_generation = 1     # +1 à chaque changement d'emplacement (passphrase changée ou réinitialisée)
revision = 1              # +1 à chaque écriture du fichier (roster compris)
mac = "base64(32)"
[kdf]
algorithm = "argon2id"
version = 19
memory_kib = 262144
iterations = 3
parallelism = 1
salt = "base64(16)"
[[keyslot]]
kind = "passphrase"       # exactement un ; "device" réservé à S3 (champs device, enc), couvert par le MAC
id = "base64(16)"
nonce = "base64(24)"
wrapped = "base64(48)"
[roster]                  # scellé sous K_roster : [{id, name, added_at, revoked}]
nonce = "base64(24)"
ciphertext = "base64(…)"
```
`deny_unknown_fields`. Version inconnue → `sync_version` (la version est lue avant l'analyse stricte).

### 3.4 Instantané `*.snap` et réplica local (livrés en S1a)
```toml
format = "portico-sync-snapshot"   # réplica : "portico-sync-replica"
version = 1
device = "hex(16)"
epoch = 1
keyset_generation = 1
seq = 42                           # séquence de publication (réplica : génération locale)
nonce = "base64(24)"
ciphertext = "base64(…)"
```
Corps déchiffré (JSON) : `{format, bundle_id, epoch, keyset_generation, device, seq, content}`. Contenu d'un instantané :
`device_name` (validé, F16), `app_version`, `published_at` (affichage), `clock`, `seen`, `state` (contexte causal global +
enregistrements). Contenu du réplica (`data_dir/sync/replica.snap`, sous K_local) : nom, `clock`, `publish_seq`, état et
marques hautes, **dans un seul fichier atomique** (F5). Champs inconnus du corps ignorés (ajouts dans la v1) ; enregistrements
non compris conservés tels quels (§4).

---

## 4. Sémantique de fusion (livrée en S1a)

### 4.1 Horloges
- **Point** `(appareil, compteur)` : une écriture. Les compteurs ne sont jamais réutilisés : `LocalClock` refuse d'émettre dès
  qu'il a observé un contexte ou une horloge publiée de son propre appareil plus avancés (F5 ; test
  `restored_data_dir_never_reuses_a_dot`). Le compteur est persisté avant toute publication.
- **Vecteur de versions** : appareil → plus haut compteur connu. C'est le **contexte causal global** de l'état, jamais collecté
  (F6). Les instantanés étant complets, il reste contigu.
- **Registre multi-valeurs** : les points survivants d'une clé, avec valeur et horodatage d'affichage. Fusion : garder un point
  présent des deux côtés, ou présent d'un seul côté et non couvert par le contexte de l'autre ; un point couvert mais absent a
  été écrasé (ou collecté) là-bas. Les valeurs égales sont **regroupées** pour l'affichage sans perdre de point, ce qui garde la
  fusion commutative, associative et idempotente (tests de propriétés).
- **Écriture locale** : nouveau point qui remplace tous les points survivants (tous dans le contexte de l'écrivain).
- **Conflit** ⇔ plus d'une valeur distincte. **Résolution** = écriture locale explicite (choix d'un côté, ou « Union » pour les
  tags), jamais automatique.
- Un même point reçu avec une autre valeur (impossible pour un appareil honnête) est **signalé** (`divergent_dots`), la valeur
  déjà vérifiée est gardée.

### 4.2 Clés d'enregistrement
| Clé | Valeur | Granularité |
|---|---|---|
| `Host { alias }` | `{hostname, user, port, identity_file, proxy_jump, renamed_from}` ou pierre tombale | tout le tuple de connexion |
| `HostMeta { alias, field }`, field ∈ tags, group, favorite, notes | ensemble de tags / texte / booléen ; pierre tombale = champ vidé | par champ ; pour **tous** les alias, y compris ceux de `~/.ssh/config` (décision #7) |
| `Snippet { id }` | snippet entier ou pierre tombale | tout le snippet |
| `Setting { key }` | texte | par clé, par action de keymap |
| `Secret { account }` | secret scellé — **réservé à S2**, mots de passe **liés** seulement (F3) | tout l'enregistrement |

`identity_file` : chemin `~/…` à séparateurs `/` dont toutes les composantes sont normales (pas de `..`, `.`, racine, lecteur,
`:`, `\`, UNC, caractère de contrôle) (F10). Un chemin hors du dossier personnel n'est pas synchronisé (issue).

### 4.3 Réglages synchronisés
| Réglage | Synchronisé ? | Pourquoi |
|---|---|---|
| `theme`, `dark_theme` | oui | goût de l'utilisateur |
| `keymap.tui.*`, `keymap.gui.*` | oui (la GUI valide les keymaps reçues comme la TUI, F18) | liaisons abstraites, portables |
| `gui.copy_on_select` | **non** (F18) | touche au presse-papiers |
| `updates.check` | **non** | consentement réseau par installation (ADR du 2026-10-23) |
| fenêtres, chemins, niveau de log, config de sync, clés inconnues | non | propres à la machine |

### 4.4 Suppressions et pierres tombales
- Une suppression est une valeur `Tombstone` sous un nouveau point : édition et suppression concurrentes = **conflit**.
- **Collecte** : un registre dont **toutes** les valeurs sont des pierres tombales couvertes par le contexte stable (minimum des
  contextes des appareils non révoqués) est retiré. Un conflit impliquant une valeur vivante n'est **jamais** collecté.
- **Contexte global** (F6) : un enregistrement absent d'un côté dont les points sont couverts par le contexte de ce côté est
  supprimé, pas ressuscité, même s'il revient par un vieil instantané (test
  `gc_tombstone_not_resurrected_by_new_joiner_on_rolled_back_folder`).
- Horizon de repli : **180 jours** (décision #9), mesuré sur l'heure **locale** de la dernière nouvelle séquence observée, jamais
  sur `published_at` (F17). Un appareil revenu après l'horizon passe par une fusion de retour : ce qu'il est seul à détenir est
  listé (« restaurer ou abandonner »), jamais republié en silence.

### 4.5 Renommages
Pierre tombale sur `Host{ancien}` + `Host{nouveau}` avec `renamed_from`. Renommage concurrent d'une édition : conflit sur
`Host{ancien}` (appliquer l'édition au nouveau nom, restaurer, accepter). Deux renommages concurrents : deux nouveaux
enregistrements, signalés comme doublon possible.

### 4.6 Modifications locales, y compris à la main (S1b)
Chaque passe **projette** les fichiers locaux en enregistrements et compare à la dernière projection appliquée ; toute
différence est une écriture locale. La projection garde l'état **non appliqué** des clés retenues, en quarantaine, masquées ou
exclues : seule la suppression observée d'une valeur **réellement écrite** produit une pierre tombale (F7). `hosts.conf` non
gérable → sync des hôtes suspendue (`sync_hosts_file_unmanaged`), fichier intact. Imports : enregistrements ordinaires, la
provenance reste locale.

### 4.7 Hôte synchronisé contre `~/.ssh/config` (S1b, F1)
- Tout hôte reçu passe par les contrôles de masquage de l'import (`shadows_a_name`, `looks_like_other_host`, localhost, hôtes de
  rebond, noms d'hôtes) → quarantaine `sync_alias_shadowed`.
- **Tout nouvel hôte et tout changement de HostName / Port / User / ProxyJump / IdentityFile reçu est retenu dans le réplica**
  et n'est écrit dans `hosts.conf` qu'après **confirmation sur cet appareil** (comme `ImportOptions::allow_redirect`) ;
  « Préférer le distant » / « Préférer le local » ne valent jamais confirmation (décision #5 révisée : confirmation stricte par
  hôte et par appareil).
- ProxyJump vers un alias inconnu : retenu (`sync_jump_missing`) ; IdentityFile absent : écrit, avertissement
  (`sync_key_missing`) ; listes blanches existantes non respectées : quarantaine (`sync_record_invalid`, nomme le type et le
  champ, jamais la valeur). Collisions d'alias à la casse près : contrôlées à l'application par les règles de `managed_hosts`.
- **Snippets** modifiés par la sync : badge, et le premier lancement après un changement reçu montre le corps complet et demande
  confirmation (F8).

### 4.8 Mots de passe (S2)
`IndexedSecretStore` (comptes non secrets indexés à chaque `set`/`delete`), liaison à l'empreinte de clé hôte préservée : B
n'envoie un mot de passe que si sa propre vérification donne la même clé ; l'invite TOFU de B peut afficher en lecture seule
« correspond / DIFFÈRE de la clé vérifiée sur <appareil> » (jamais d'écriture dans `known_hosts`, jamais de raccourci
d'acceptation). Mots de passe **non liés jamais synchronisés** (F3, décision #4 révisée). « Inclure les mots de passe » est un
réglage **du bundle**, **désactivé par défaut** (décisions #3 révisée et F12). `KeyPassphrase` jamais en v1. Application
impossible (vault verrouillé) → en attente, scellé, `sync_secrets_pending(n)`.

### 4.9 Deux processus Portico (TUI + GUI)
Verrou exclusif `data_dir/sync/sync.lock` de la projection à la publication ; ordre : `sync.lock` puis les verrous par fichier.

---

## 5. Intégration au moteur (S1b)

### 5.1 Modules (`crates/portico-core/src/sync/`)
| Fichier | Rôle | État |
|---|---|---|
| `mod.rs` | `SyncError` (codes `sync_*`, indices), invariants | ✅ S1a |
| `crypto.rs` | KDF bornée, HKDF, scellement, encodage canonique, MAC, remplissage | ✅ S1a |
| `bundle.rs` | `bundle.toml` : créer, ouvrir (passphrase ou DK), changer / réinitialiser la passphrase, roster | ✅ S1a |
| `container.rs` | instantanés et réplica scellés | ✅ S1a |
| `clock.rs` | `DeviceId`, `DeviceName`, points, vecteurs, registres, `LocalClock` | ✅ S1a |
| `model.rs` | clés, valeurs, `SyncState`, fusion, conflits, collecte, passage opaque | ✅ S1a |
| `guard.rs` | marques hautes, retour arrière, fourche, rétrogradation, restauration, retenues | ✅ S1a |
| `passphrase.rs` | générateur EFF, estimation, minimum imposé | ✅ S1a |
| `folder.rs` | `trait FolderIo` + `StdFolder`, candidats, placeholders, racine sûre (F9) | S1b |
| `project.rs` / `apply.rs` | projection des fichiers locaux, application ordonnée et validée | S1b |
| `state.rs` / `task.rs` | `data_dir/sync/` (`sync.toml` non secret, `replica.snap`), boucle de fond | S1b |

`Paths::sync_dir()` = `data_dir/sync` (local, jamais itinérant). Ordre d'une passe, sûr en cas de crash : fusion en mémoire →
persistance des points et du réplica (**un point n'est jamais publié avant d'être durable localement**) → application locale
(idempotente) → projection appliquée → publication. Au démarrage et à chaque passe, **avant toute écriture**, lecture du fichier
propre de l'appareil (`check_own_file`) : en avance → rejoindre sous un nouvel id.

### 5.2 API du moteur (toutes `async`, travail bloquant hors runtime)
`sync_status`, `sync_probe_folder`, `sync_create`, `sync_join_preview` (n'écrit rien ; jeton **à usage unique avec expiration**,
DK en `Zeroizing`, F14), `sync_join_commit` (ReviewEach | PreferRemote | PreferLocal), `sync_unlock`, `sync_now`,
`sync_conflicts`, `sync_resolve` (Pick | Union pour les tags | KeepBoth pour les snippets | Value), `sync_issues`,
`sync_skip_on_this_device`, `sync_change_passphrase`, `sync_reset_passphrase`, `sync_devices` (nom toujours accompagné du
préfixe d'id, F16), `sync_rename_device`, `sync_set_options`, `sync_disable` ; S2 : `sync_rotate_keys`, `sync_forget_device`.
Statuts : Disabled, Locked, Idle, Syncing, Waiting, FolderUnavailable, Alert (Rollback | Fork | BundleTampered |
KeysRotatedElsewhere | PassphraseChangedTwice | PassphraseReset).

Codes d'erreur (livrés en S1a) : `sync_wrong_passphrase`, `sync_passphrase_weak`, `sync_corrupt`, `sync_version`,
`sync_kdf_out_of_bounds`, `sync_kdf_memory`, `sync_bundle_tampered`, `sync_snapshot_invalid`, `sync_rollback`, `sync_fork`,
`sync_restored`, `sync_too_large`, `sync_device_name_invalid`, `sync_record_invalid`, `sync_resolution_invalid`, `sync_crypto`.
S1b ajoute ceux des E/S et de l'application (`sync_folder_missing`, `sync_folder_not_portico`, `sync_folder_unwritable`,
`sync_locked`, `sync_hosts_file_unmanaged`, `sync_alias_shadowed`, `sync_jump_missing`, `sync_key_missing`). Aucun message ne
contient de valeur ni de secret.

### 5.3 Événements
Nouveaux : `EngineEvent::Sync { status }`, `SyncConflicts { count }`, `SnippetsChanged`, `SettingsChanged { settings }` ;
existants : `HostsChanged`, `HostMeta`. Après `Lagged`, les UIs relisent `sync_status` / `sync_conflicts`.

### 5.4 Ordonnancement
Sondage toutes les 30 s (`stat` de `bundle.toml` + listage de `devices/`), passe immédiate au démarrage, sur `sync_now` et au
réveil ; publication différée de 5 s après le dernier changement local (au plus une par 30 s, plus à l'arrêt propre). Pas de
surveillance de fichiers en S1 (peu fiable sur les arborescences gérées par les fournisseurs). E/S du dossier sur un **thread
dédié** alimenté par un canal borné, délai de 60 s par opération ; Argon2 en `spawn_blocking` ; tâche sous un
`CancellationToken` enfant de l'arrêt.

### 5.5 Situations particulières
Dossier absent → `FolderUnavailable { Missing }`, jamais recréé automatiquement ; `bundle.toml` absent → aucune écriture, choix
« attendre » ou « recréer depuis cet appareil » (passphrase, même id, nouvelle génération, avertissement) ; dossier en lecture
seule → fusion entrante seulement ; FAT/exFAT ou partage réseau sans ACL : permis, resserrement des droits ignoré.

### 5.6 Plateformes
Linux : `0600` / `0700` pour ce que Portico crée, le dossier choisi n'est jamais modifié. macOS : placeholders `.icloud`
détectés ; la TUI non signée peut faire demander le trousseau. Windows : DACL propriétaire via `winfs` (échec toléré hors NTFS),
chemin conservé tel que choisi, suffixe OneDrive reconnu, état et DK en `%LOCALAPPDATA%` / `Local` (jamais dans le profil
itinérant : cas E4).

---

## 6. Parcours UI (S1c, haut niveau)

Charger `ui-design-premium`, `tui-patterns` et `gui-tauri-patterns` avant. La palette expose « Sync… ».
1. **Assistant** : explication (« seuls des fichiers chiffrés quittent la machine ; clés privées et known_hosts jamais
   synchronisés ») ; choix du dossier (GUI : `tauri-plugin-dialog`, capability `dialog:allow-open` dossiers seulement ; TUI :
   saisie avec complétion) ; **premier appareil** : passphrase + confirmation, jauge, « Générer une passphrase » (6 mots EFF),
   **minimum imposé par le cœur** (≥ 60 bits estimés ou ≥ 5 mots de la liste), nom d'appareil, « Mémoriser la clé sur cet
   appareil » (oui) ; **rejoindre** : passphrase puis revue (N hôtes, M snippets, K réglages entrants, conflits), rien n'est écrit
   avant confirmation ; écran final d'avertissement de récupération avec « J'ai compris » explicite.
2. **Barre de statut** : glyphe monochrome + libellé (`Synced 2 min ago`, `Syncing…`, `Waiting for <device>`,
   `Sync folder unavailable`, `Locked`) ; conflits et alertes en couleurs de statut seulement.
3. **Panneau Sync** : appareils (nom + préfixe d'id, dernière publication, celui-ci, inactif), options, « Synchroniser »,
   « Changer la passphrase » (qui dit ne pas protéger d'une passphrase déjà fuitée, F2), « Désactiver » (oublie la DK par
   défaut). **Conflits** : diff par champ, choix d'un côté, Union présélectionnée pour les tags, « Garder les deux » pour les
   snippets. **Issues** : alias masqué, rebond manquant, clé absente, quarantaine, secrets en attente, chacune avec un indice.
4. **Liste des hôtes** : badge « modifié par <appareil> » et confirmation requise avant la première écriture d'un changement de
   destination (F1).
5. **Récupération** : passphrase perdue mais DK mémorisée → réinitialisation depuis cet appareil (confirmation + alerte bloquante
   ailleurs) ; passphrase perdue sans DK nulle part → contenu du dossier **irrécupérable** (par conception), données locales
   intactes, « nouveau bundle » possible ; désactiver ne supprime jamais les données locales.

---

## 7. Stratégie de test

**Livré en S1a (81 tests unitaires et de propriétés dans `sync::`)** :
- `crypto` : vecteurs RFC 5869 (HKDF), RFC 4231 (HMAC) et draft XChaCha ; bornes KDF refusées **sans aucune dérivation**
  (compteur) en dessous du plancher et au-dessus du plafond ; refus d'Argon2 → erreur ; sous-clés distinctes et liées au bundle
  et à l'époque ; encodage canonique non ambigu ; 10⁴ nonces uniques ; chaque octet altéré refusé ; seaux de remplissage ;
  `Debug` caviardé.
- `bundle` : aller-retour ; mauvaise passphrase et altération de chaque champ lié à l'emplacement indiscernables ; altération
  après déballage → `sync_bundle_tampered` ; un octet modifié n'importe où → refus ; versions, format, champs inconnus,
  plafond de taille ; paramètres hostiles refusés avant dérivation ; passphrase faible refusée à la création et au changement
  avant toute dérivation ; changement (même DK, génération + 1, ancien emplacement non transplantable) ; limite F2 documentée ;
  réinitialisation avec la DK ; roster (révision + 1, doublons, plafond) ; emplacements S3 conservés et authentifiés.
- `container` : aller-retour ; chaque champ de l'AAD lié (séquence, génération, appareil, époque, bundle, clés, longueur
  complétée, type de conteneur) ; échange d'appareil refusé ; octets altérés ; remplissage ; plafonds ; contenu authentifié mais
  incohérent refusé ; réplica sous sa propre clé.
- `clock` / `model` : frères concurrents, valeurs égales fusionnées, pierre tombale contre édition = conflit, Union, résolutions
  vérifiées, collecte seulement stable et jamais d'un conflit, `gc_tombstone_not_resurrected_by_new_joiner_on_rolled_back_folder`,
  `restored_data_dir_never_reuses_a_dot`, point divergent signalé, liste blanche des réglages, validation des écritures,
  `synced_identity_file_rejects_parent_and_unc`, passage opaque plafonné, invariants structurels ; **proptest** : commutativité,
  associativité, idempotence, convergence quel que soit l'ordre (avec doublons), aucune écriture perdue sauf écrasée, conflit ⇔
  écritures concurrentes de valeurs différentes.
- `guard` : retour arrière et fourche par appareil, `older_bundle_toml_is_refused_after_passphrase_change`, cliquet de format,
  état local restauré, mises à jour retenues, ancienneté sur l'heure locale.
- `passphrase` : liste EFF (7776 mots, triée, SHA-256 épinglé), générateur (6 mots, uniforme, χ² sur 16 classes), rejet de la
  zone biaisée, seuil de force, coût borné sur entrée longue.

**À venir** : S1b — `project`/`apply` sur `Paths::rooted` (édition à la main, `hosts.conf` non géré, alias masqué, rebond
manquant, IdentityFile, snippet avec ESC en quarantaine, `updates.check` jamais synchronisé) et les tests nommés par la revue :
`sync_apply_refuses_hostname_like_alias`, `sync_destination_change_held_until_confirmed`,
`join_prefer_remote_does_not_confirm_redirects`, `quarantined_or_held_record_never_becomes_tombstone`,
`synced_snippet_change_requires_confirmation_before_run`, `devices_dir_symlink_is_refused_and_nothing_written_outside` ;
intégration multi-moteurs avec un `SimulatedProvider` (visibilité retardée, écritures tronquées, copies en conflit de chaque
fournisseur, retour arrière, suppression, placeholders, réordonnancement, lecture seule, dossier absent, conflit de
`bundle.toml`) ; injection de crash entre chaque étape de §5.1 ; fourche par clonage. S2 —
`rotation_makes_new_snapshots_unreadable_with_old_dk`, `unbound_password_is_never_projected`,
`bound_password_not_offered_for_other_key_on_b`, scénario avec serveur russh en process. Hygiène des secrets (motif
`"hunter2"`) étendue aux types et DTO de sync. Cibles `cargo-fuzz` optionnelles (bundle, enveloppe, corps). CI Windows (DACL,
noms OneDrive, chemins longs). E2E bureau « deux profils, un dossier ».

**Points de revue `agent-security`** : conception (fait, §12) ; après crypto + bundle + folder (S1a + `folder.rs` de S1b) ;
après S2 ; clôture de phase avec mise à jour de `SECURITY.md` (fuites de métadonnées A1, appareil compromis A5, limite du
premier rattachement, gel, dossier partagé = clé partagée). `agent-design-review` : assistant, barre de statut, conflits et
issues dans les 4 thèmes, TUI 80×24, GUI 880–1200 px.

---

## 8. Découpage

| Étape | Contenu | Critère de sortie |
|---|---|---|
| **S0** (repérage) | Mesurer Dropbox, iCloud Drive, OneDrive, Syncthing, Google Drive et un dépôt git : atomicité des renommages, nommage des conflits, fichiers temporaires ignorés, placeholders | Tableau des comportements observés dans ce document, fixtures du `SimulatedProvider` |
| **S1a** ✅ | Fondations pures : crypto, `bundle.toml`, conteneurs scellés, horloges et fusion, gardes, passphrase, erreurs | 81 tests verts, clippy propre (Linux et cible Windows), `cargo deny` vert |
| **S1b** | Dossier (`FolderIo`, racine sûre), projection, application validée (quarantaine, masquage, confirmation des destinations), état local, tâche de fond, statut et événements, API create/join/unlock/resolve/change/reset/disable | Deux moteurs sur un dossier partagé convergent, conflit hors ligne compris ; scénarios du `SimulatedProvider` verts |
| **S1c** | Assistant, barre de statut, panneau Sync, conflits, issues, badges (TUI + GUI) | Deux machines partagent hôtes, métadonnées, snippets et thème, conflit montré et résolu dans les deux UIs |
| **S2** | **Rotation des clés et « oublier un appareil » d'abord**, puis mots de passe liés (indexation, couche interne, attente si verrouillé, mention TOFU, détection par K_cmp) | Révocation + rotation depuis un appareil restant ; mot de passe mémorisé sur A proposé sur B seulement pour la même clé hôte vérifiée |
| **S3** | Emplacements X25519 par appareil, signatures Ed25519, réglage de l'horizon, exclusions par réglage, options (`KeyPassphrase`, indices `known_hosts` jamais écrits) | Exercice d'appareil perdu sans ressaisir la passphrase |

Mises à jour de contrat avec S1b/S1c : `CLAUDE.md` §2/§4 (module `sync/` ; ligne secrets : « synchronisés seulement re-chiffrés,
opt-in »), `SECURITY.md`, `docs/KEYBINDINGS.md`.

---

## 9. Décision (résumé de l'ADR)

Voir `docs/STACK.md`, 2026-10-27. Alternatives écartées : remote git intégré (client réseau dans Portico), serveur ou compte
Portico (verrouillage), `age`/`rage` (scrypt sans bornes, pas de liaison de contexte ; format X25519 à reconsidérer pour S3),
`sops` (outil externe, clés en clair), libsodium secretstream (dépendance C, flux), fichier unique, un fichier par
enregistrement, journal par appareil, `automerge`/`yrs` (lourds), `crdts` (format qui fuirait dans le nôtre ; garde son rôle
d'oracle de test possible), dernier écrivain gagnant à l'horodatage (perte silencieuse). À revoir si le repérage S0 montre des
renommages non atomiques, si le partage sélectif est demandé, ou si la rotation sans passphrase devient nécessaire avant S3.

---

## 10. Écarts de l'implémentation S1a par rapport au document de conception

| Écart | Raison |
|---|---|
| Champ `revision` ajouté à `bundle.toml` (+1 à chaque écriture), la marque haute du bundle compare (époque, keyset_generation, révision) | `keyset_generation` est lié à l'AAD de l'emplacement (F4) : une mise à jour du roster faite avec la seule DK ne peut pas l'incrémenter sans la passphrase. Sans compteur propre, une ancienne version du roster serait indétectable |
| `keyset_generation` aussi lié à l'AAD des instantanés et du réplica | Demandé par la liste de liaison de S1a ; la politique (≤ génération courante) se décide en S1b |
| Registres stockés par point (pas de contexte par registre), avec un seul contexte global | Le contexte global (F6) rend le contexte par registre redondant ; le regroupement des valeurs égales garde tous les points, ce qui préserve les lois de fusion (une fusion qui jetait des points rendait la fusion non associative) |
| Passage opaque : tout enregistrement non lu exactement (genre inconnu, clé de réglage inconnue, forme de valeur inconnue, chemin d'identité invalide) est conservé tel quel ; union d'octets plafonnée à 256 Kio, dépassement compté, action « purger » | Simple, sûr (jamais appliqué) et commutatif ; les versions remplacées s'accumulent jusqu'au plafond, une version plus récente de Portico les fusionne |
| Alias gardés tels qu'écrits dans les clés | Les collisions à la casse près se contrôlent à l'application avec les règles existantes de `managed_hosts` (S1b) |
| Estimation : modèle diceware (`mots distincts × log2 7776`) quand tous les mots sont de la liste, `zxcvbn` sinon | `zxcvbn` surestime quatre mots de la liste (≈64 bits contre 51,7 réels) |
| Code `sync_snapshot_invalid` pour un instantané non authentifié, distinct de `sync_corrupt` | Un fichier en cours d'envoi ou tronqué (E2) doit pouvoir être traité comme « en attente » par S1b |
| Plafond KDF 512 Mio / t=6 (au lieu de 1 Gio / t=10) | F13 |
| Roster limité à 64 appareils | Tenir dans le plafond de 64 Kio de `bundle.toml` avec des noms de 64 caractères |

### Ce que S1b attend de S1a
`bundle::{create, open, open_with_key, rewrap, change_passphrase, with_roster}`, `OpenedBundle { header, roster, data_key, keys,
fingerprint }` ; `container::{seal_snapshot, open_snapshot, seal_replica, open_replica, Binding, SnapshotContent,
ReplicaContent}` ; `model::{SyncState, RecordKey, RecordValue, validate_record, Resolution, MergeReport}` ;
`clock::{DeviceId, DeviceName, LocalClock, VersionVector}` ; `guard::{HighWater, check_own_file, withheld, STALE_HORIZON_SECS}` ;
`passphrase::{generate_passphrase, assess_passphrase}` (publics, pour les jauges des UIs). Les `#[allow(dead_code)]` de
`sync/mod.rs` tombent avec S1b.

---

## 11. Décisions du propriétaire (2026-10-27)
1. KDF fort : m=256 Mio, t=3, plancher 128 Mio (plafond ramené à 512 Mio / t=6 par F13).
2. DK mémorisée par défaut (trousseau), option « ne pas mémoriser » pour les machines partagées.
3. Mots de passe : ~~cochés par défaut~~ → **décochés par défaut, opt-in au niveau du bundle** (révisé après la revue).
4. Mots de passe non liés : ~~seulement sans mot de passe lié~~ → **jamais synchronisés** (révisé, F3).
5. Changement de destination reçu : ~~badge + note TOFU~~ → **confirmation stricte par hôte et par appareil** (révisé, F1).
6. Liste blanche des réglages : `updates.check` jamais ; ~~`gui.copy_on_select` synchronisé~~ → **non synchronisé** (F18).
7. Métadonnées des hôtes de `~/.ssh/config` : synchronisées.
8. Réinitialisation sans l'ancienne passphrase depuis un appareil détenant la clé : permise derrière une confirmation, avec une
   **alerte bloquante** à acquitter sur tous les autres appareils.
9. Horizon des pierres tombales : 180 jours.
10. Tags concurrents : toujours un conflit, « Union » présélectionnée, jamais silencieux.
11. Passphrase : générateur (6 mots EFF, liste embarquée), jauge, minimum imposé (≈60 bits estimés ou ≥ 5 mots).
12. S3 : plus tard. **Rotation des clés et « oublier un appareil » livrés avec S2, avant tout mot de passe synchronisé.**
13. Ce document en français.

---

## 12. Exigences de la revue de sécurité (agent-security, 2026-10-27)

| # | Gravité | Exigence | État |
|---|---|---|---|
| F1 | ÉLEVÉ | Hôtes reçus soumis aux contrôles de masquage de l'import (quarantaine `sync_alias_shadowed`) ; nouvel hôte et changement de HostName/Port/User/ProxyJump/IdentityFile retenus jusqu'à confirmation sur l'appareil ; PreferRemote/PreferLocal ne confirment jamais | S1b |
| F2 | ÉLEVÉ | En S1, « abandonner ce bundle et en créer un nouveau » est le seul remède ; l'UI de changement dit qu'il ne protège pas d'une passphrase déjà fuitée ; rotation + oubli d'appareil en S2 avant les mots de passe ; recommander de changer les mots de passe serveurs déjà synchronisés après rotation | limite testée en S1a ; UI S1c ; rotation S2 |
| F3 | ÉLEVÉ | Jamais de mot de passe non lié ; liés seulement, envoyés par B seulement si sa clé vérifiée = l'empreinte liée ; mention en lecture seule à l'invite TOFU | S2 (genre `Secret` réservé en S1a, jamais écrit) |
| F4 | MOYEN | Marque haute locale du bundle, refus des rétrogradations ; keyset_generation dans l'AAD de l'emplacement ; cliquet de format | ✅ S1a |
| F5 | MOYEN | Lire son propre fichier au démarrage et à chaque passe ; s'il est en avance → nouvel id ; horloge, séquence, génération dans le réplica scellé, un seul fichier atomique | ✅ S1a (primitives) ; câblage S1b |
| F6 | MOYEN | Contexte causal global jamais collecté ; un enregistrement absent couvert par le contexte est supprimé, pas ressuscité | ✅ S1a |
| F7 | MOYEN | La projection garde l'état non appliqué des clés retenues / en quarantaine / masquées / exclues ; seule la suppression d'une valeur réellement écrite donne une pierre tombale | S1b |
| F8 | MOYEN | Snippet modifié par la sync : badge, corps complet et confirmation au premier lancement | S1b/S1c |
| F9 | MOYEN | Racine canonisée ; contrôle `symlink_metadata` à chaque passe ; points d'analyse Windows ; exclusion de `~/.ssh`, config, données | S1b |
| F10 | MOYEN | IdentityFile : composantes normales seulement sous `~` | ✅ S1a |
| F11 | MOYEN | Détection de changement de secret par compteur de génération d'index ; HMAC(K_cmp, compte ‖ secret) dans l'enregistrement scellé | S2 (K_cmp dérivée en S1a) |
| F12 | MOYEN | « Inclure les mots de passe » est un réglage du bundle, présenté comme tel | S2 |
| F13 | FAIBLE | Bornes KDF propres (défaut 256 Mio t=3 p=1, plancher 128 Mio, plafond 512 Mio t=6 p=4) ; `try_reserve` + `hash_password_into_with_memory` ; (paramètres, sel) retenus localement | ✅ S1a |
| F14 | FAIBLE | Sous-clés K_roster et K_local distinctes ; MAC sur encodage canonique préfixé ; comparaison en temps constant ; aucune écriture conditionnée au déballage avant vérification du MAC ; jeton de rattachement à usage unique avec expiration, DK en `Zeroizing` ; DK mémorisée en base64 `Zeroizing`, élément macOS non synchronisable ; « Désactiver » oublie la DK par défaut | ✅ S1a (crypto) ; reste S1b |
| F15 | FAIBLE | ≤ 8 candidats par appareil, budget d'octets par passe, pause sans suppression au-delà de la limite de listage ; passage opaque plafonné (256 Kio) + « purger » | ✅ S1a (opaque) ; dossier S1b |
| F16 | FAIBLE | Nom d'appareil validé (pas de contrôle / bidi / invisible), toujours affiché avec le préfixe d'id | ✅ S1a |
| F17 | FAIBLE | Ancienneté mesurée sur l'heure locale d'observation de la dernière nouvelle séquence, jamais `published_at` | ✅ S1a |
| F18 | FAIBLE | `copy_on_select` non synchronisé ; la GUI valide les keymaps reçues comme la TUI ; force de passphrase imposée dans le cœur (`sync_passphrase_weak`) à la création, au changement et à la réinitialisation | ✅ S1a (cœur) ; GUI S1c |
