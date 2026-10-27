# Synchronisation chiffrée par dossier (phase 11)

> **État : S1a livré (fondations pures dans `portico-core/src/sync/`) ; S1b livré (intégration au moteur : dossier, projection,
> application validée, passe, tâche de fond, API `Engine::sync_*`, événements) ; S1c (UIs) à venir ; S2 et S3 ensuite.**
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
- **Retour arrière / rejeu** : chaque appareil garde, dans son état local scellé, la plus haute séquence et l'empreinte du
  contenu authentifié vues par appareil et pour `bundle.toml`. Plus bas = retour arrière ; même séquence, autre contenu = fourche. Un attaquant du dossier
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
- **Limite** (revue S1a, constat 6) : sous Linux avec surallocation (*overcommit*), la réservation réussit presque toujours et
  les pages ne sont engagées qu'au remplissage ; une machine à court de mémoire peut alors échanger (*swap*) ou rencontrer
  l'OOM killer au lieu de recevoir `sync_kdf_memory`. Ce qui borne le dommage est le plafond vérifié avant toute allocation
  (512 Mio au total, quel que soit p) : un `bundle.toml` hostile ne peut pas demander plus. Pas de sonde de mémoire libre à
  l'exécution (racée et propre à chaque plateforme).
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
fusionnées par appareil). Localement, **dans le réplica scellé** (F5), l'appareil garde par appareil (seq max, horloge publiée
par l'appareil lui-même, empreinte, heure locale d'observation) et pour le bundle (époque, keyset_generation, révision,
empreinte).

**Empreinte** (revue S1a, constat 1) : SHA-256 du **contenu authentifié** seulement, jamais des octets du fichier. Instantané
et réplica : AAD, nonce et chiffré décodés (`container::Sealed` / `Opened`) ; `bundle.toml` : entrée canonique du MAC et MAC.
Un commentaire, des blancs, l'ordre des clés ou des fins de ligne CRLF ajoutés par le fournisseur (ou par un attaquant sans la
clé) ne changent rien : ils ne peuvent ni simuler une fourche (et forcer un nouveau rattachement) ni une divergence du bundle.
Un rescellement du même contenu (nonce neuf) change l'empreinte (test `fingerprint_ignores_unauthenticated_formatting`).

| Observation | Verdict | Action |
|---|---|---|
| fichier valide avec `seq` < marque | retour arrière | ignoré, `sync_rollback` (nomme l'appareil) |
| même `seq`, autre empreinte | fourche | ignoré, `sync_fork` ; si c'est cet appareil : rejoindre sous un nouvel id |
| `bundle.toml` avec (époque, génération, révision) inférieur | retour arrière (F4) | refusé, `sync_rollback` |
| même triplet, autre contenu | réécritures concurrentes | choix déterministe du gagnant parmi les copies (S1b) |
| format plus ancien qu'un format déjà vu | rétrogradation | refusé (cliquet de version) |
| un `seen[d]` > la `seq` présente pour d | retenu ou pas encore synchronisé | « en attente », avertissement après 24 h |
| le contexte publié par A affirme `context[d]` > l'horloge que d a publiée lui-même (d ≠ A), ou d absent du roster | contexte gonflé (A5) ou instantané de d pas encore arrivé (revue S1a, constat 3) | instantané de A **différé**, pas fusionné (`guard::check_context_claims`) ; alerte nommant A s'il persiste |
| un autre appareil affirme des points de **cet** appareil au-delà de son compteur | affirmation d'un tiers | alerte `sync_third_party_claim` nommant l'auteur ; **jamais** de nouveau rattachement automatique (seuls les fichiers propres de l'appareil prouvent une restauration) |
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
d'un genre ou d'une forme non compris : registres génériques fusionnés et relayés causalement, jamais appliqués (§4.10). Un
conteneur scellé sous une `keyset_generation` supérieure à celle du bundle vérifié est refusé (`sync_corrupt`, revue S1a,
constat 6). Le JSON du corps (et du roster) est écrit directement dans un tampon `Zeroizing` dimensionné d'avance pour son seau
et l'étiquette : aucune réallocation ne laisse de fragment de clair en mémoire libérée.

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
- Une valeur **vivante** retirée parce que le contexte de l'autre côté la couvre alors que l'autre côté n'a **aucune** entrée
  pour la clé est comptée (`dropped_live_without_successor`, revue S1a, constat 3). Hors collecte d'une pierre tombale stable
  (ou après l'horizon d'ancienneté), seul un contexte gonflé la produit.
- **Fusion transactionnelle** (revue S1a, constat 2) : `SyncState::merge` calcule sur une copie, vérifie les invariants (20 000
  enregistrements, 1024 appareils de contexte, 64 valeurs par registre) et la taille encodée (≤ seau maximal moins une marge pour
  le reste de l'instantané) ; en cas d'échec, `sync_merge_refused` nomme l'appareil source et l'état local n'est pas touché. Un
  instantané valide d'un appareil compromis ne peut donc pas rendre le nôtre impubliable ou illisible.
- **Restauration** : `LocalClock` ne conclut à un état restauré (`sync_restored`) que sur les fichiers **propres** de l'appareil
  (son instantané dans le dossier, son réplica) ; un contexte publié par un tiers est vérifié par `check_claim`, qui alerte sans
  rien changer (constat 3).

### 4.2 Clés d'enregistrement
| Clé | Valeur | Granularité |
|---|---|---|
| `Host { alias }` | `{hostname, user, port, identity_file, proxy_jump, renamed_from}` ou pierre tombale | tout le tuple de connexion |
| `HostMeta { alias, field }`, field ∈ tags, group, favorite, notes | ensemble de tags / texte / booléen ; pierre tombale = champ vidé | par champ ; pour **tous** les alias, y compris ceux de `~/.ssh/config` (décision #7) |
| `Snippet { id }` | snippet entier ou pierre tombale | tout le snippet |
| `Setting { key }` | texte | par clé, par action de keymap |
| `Secret { account }` | secret scellé — **réservé à S2**, mots de passe **liés** seulement (F3) | tout l'enregistrement |

`identity_file` : chemin `~/…` à séparateurs `/` dont toutes les composantes sont normales (pas de `..`, `.`, racine, lecteur,
`:`, `\`, UNC, caractère de contrôle) (F10), au moins aussi strict que ce que Portico écrit dans `hosts.conf`
(`managed_hosts::check_path_chars` : ni guillemet, ni `%`, `$`, `#`, `=`, accent grave, barre oblique inverse) et sans
composante que Windows réserve ou réécrit (`store::windows_name_problem` : `CON`, `NUL`, `COM0`–`COM9`, `LPT0`–`LPT9`…, point ou
espace final, `<>:"|?*`), sur toutes les plateformes (revue S1a, constat 6). S1b passe **chaque valeur reçue** par
`validate_record` avant de l'appliquer. Un chemin hors du dossier personnel n'est pas synchronisé (issue).

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
- **Règle** : un registre qui disparaît par couverture ne donne **jamais** de suppression locale. Seule une valeur `Tombstone`
  explicite supprime un enregistrement local (S1b applique des pierres tombales, jamais des absences).
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

### 4.10 Compatibilité ascendante (revue S1a, constat 4)
- La couche de fusion ne voit que des **registres génériques** : clé = JSON canonique (compact, clés triées) d'un objet doté d'un
  `kind` ; valeur = JSON canonique quelconque. Tous les genres, connus ou non de cette version, fusionnent avec les mêmes points
  et contextes. Les valeurs typées ne sont décodées qu'à la projection (`SyncState::project`).
- Un registre dont la clé ou une valeur ne se décode pas exactement (genre inconnu, clé de réglage inconnue, champ inconnu :
  le décodage typé garde `deny_unknown_fields`) est **retenu** (`RegisterView::Held`) : conservé, fusionné et relayé
  causalement (un enregistrement inconnu modifié deux fois n'est relayé qu'une fois, à sa dernière valeur), signalé « nécessite
  un Portico plus récent », jamais appliqué, jamais source d'une suppression ou d'un écrasement local ; une écriture locale
  par-dessus est refusée (`sync_record_needs_newer`).
- **Ajouter un champ à un genre de valeur existant est donc sûr** : les versions plus anciennes gardent l'enregistrement comme
  « nécessite plus récent » au lieu de perdre le champ en silence.
- Deux enregistrements de même clé dans un instantané (écrivain plus récent, clé écrite sous deux formes équivalentes) voient
  leurs entrées **fusionnées** (toujours sous les plafonds) ; un même point avec deux valeurs est refusé.
- Seul un JSON sans la **forme générique** d'enregistrement (`{key: {kind, …}, entries: [{dot, at, value}]}`, champ inconnu à ces
  niveaux, clé sans `kind`) rend l'état illisible (`sync_corrupt`) : changer cette forme exige un nouveau format de corps.

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
| `model.rs` | clés, valeurs, `SyncState` (registres génériques), fusion transactionnelle, projection typée, conflits, collecte | ✅ S1a |
| `guard.rs` | marques hautes, retour arrière, fourche, rétrogradation, restauration, retenues | ✅ S1a |
| `passphrase.rs` | générateur EFF, estimation, minimum imposé | ✅ S1a |
| `folder.rs` | `trait FolderIo` + `StdFolder`, candidats, placeholders, racine sûre (F9), limites (F15) ; fournisseur simulé des tests | ✅ S1b |
| `project.rs` / `apply.rs` | projection des fichiers locaux, modifications locales (F7), application ordonnée et validée (F1, F8) | ✅ S1b |
| `state.rs` | `data_dir/sync/` (`sync.toml` non secret, `replica.snap` scellé, `confirm.toml`, `sync.lock`) | ✅ S1b |
| `runner.rs` | `SyncCore` : une passe, créer, rejoindre, déverrouiller, résoudre, confirmer, désactiver | ✅ S1b |
| `api.rs` | types publics (statut, appareils, conflits, issues, confirmations en attente) | ✅ S1b |
| `engine/sync_api.rs` | `Engine::sync_*`, thread `portico-sync`, tâche de fond, événements | ✅ S1b |

`Paths::sync_dir()` = `data_dir/sync` (local, jamais itinérant). Ordre d'une passe, sûr en cas de crash (tel que livré en S1b :
verrou → racine sûre → `bundle.toml` → réplica → fichier propre → instantanés des autres, observés avant tout jugement de
contexte → écritures locales → fusion → réplica persisté → application → collecte des pierres tombales → réplica → publication) : fusion en mémoire →
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

**Livré en S1b** (`crate::sync::api`, toutes `async`, travail bloquant sur le thread `portico-sync` ou `spawn_blocking`) :
`start_sync()` (tâche de fond), `sync_status() -> SyncOverview { status, folder, device, remember_key }`,
`sync_probe_folder(PathBuf) -> FolderProbe`, `sync_generate_passphrase() -> GeneratedPassphrase`,
`sync_create(SyncCreate) -> SyncStatus`, `sync_join_preview(SyncJoin) -> JoinPreview`,
`sync_join_commit(token, JoinBulkChoice) -> SyncStatus`, `sync_unlock(SecretString)`, `sync_now()`, `sync_conflicts()`,
`sync_resolve(id, SyncResolution::{Pick, Union, KeepBoth})`, `sync_issues()`, `sync_skip_on_this_device(key)`,
`sync_pending_hosts()`, `sync_confirm_hosts(Vec<(alias, destination_hash)>)`,
`sync_reject_hosts(Vec<(alias, destination_hash)>)`, `sync_acknowledge_alerts()`,
`sync_change_passphrase(old, NewPassphrase)`, `sync_reset_passphrase(NewPassphrase)`, `sync_devices()`,
`sync_rename_device(name)`, `sync_set_options(SyncOptions)`, `sync_disable(DisableOptions)` ; snippets (F8) :
`synced_snippets_unconfirmed() -> Vec<UnconfirmedSnippet { id, snippet, hash }>`, `confirm_synced_snippet(id, hash)`,
`snippet_input_for(id, valeurs, run, bracketed_paste)` (seule source publique des octets d'un snippet :
`snippets::snippet_input` est `pub(crate)`).
`SyncStatus` : Disabled, Locked, Idle { last_pass, devices, conflicts, issues, pending_confirmations }, Syncing,
Waiting { PartialSync | Placeholder | Deferred | Busy, device }, FolderUnavailable { Missing | NotMounted | PermissionDenied |
BundleMissing | Unsafe | TooManyEntries }, Alert { Rollback | Fork | Restored | BundleTampered | KeysRotatedElsewhere |
PassphraseChanged | ThirdPartyClaim | MergeRefused | Divergent | DroppedValues, device }, Error { summary }.

Codes d'erreur (livrés en S1a) : `sync_wrong_passphrase`, `sync_passphrase_weak`, `sync_corrupt`, `sync_version`,
`sync_kdf_out_of_bounds`, `sync_kdf_memory`, `sync_bundle_tampered`, `sync_snapshot_invalid`, `sync_rollback`, `sync_fork`,
`sync_restored`, `sync_too_large`, `sync_device_name_invalid`, `sync_record_invalid`, `sync_resolution_invalid`, `sync_crypto`.
S1b ajoute ceux des E/S et de l'application (`sync_folder_missing`, `sync_folder_not_portico`, `sync_folder_unwritable`,
`sync_locked`, `sync_hosts_file_unmanaged`, `sync_alias_shadowed`, `sync_jump_missing`, `sync_key_missing`), et, livrés aussi :
`sync_bundle_exists`, `sync_folder_unsafe`, `sync_folder_too_many`, `sync_folder_timeout`, `sync_folder_io`, `sync_disabled`,
`sync_already_enabled`, `sync_join_expired`, `sync_busy`, `sync_host_in_use`, `sync_not_pending`, `sync_snippet_unconfirmed`,
`sync_passphrase_changed`, `sync_keys_rotated`, `sync_divergent`, `sync_dropped_values`, `sync_deferred`, `sync_pending`,
`sync_third_party_claim`, `sync_merge_refused`, `sync_record_needs_newer`, `sync_local_host_unsynced`,
`sync_local_unsynced`. Chacun a un indice. Aucun message ne contient de
valeur ni de secret.

### 5.3 Événements
Nouveaux : `EngineEvent::Sync { status }`, `SyncConflicts { count }`, `SyncPendingChanged` (ce qui attend une
confirmation a changé, **contenu compris** à nombre égal), `SnippetsChanged`, `SettingsChanged { settings }` ; existants :
`HostsChanged`, `HostMeta`. Après `Lagged`, les UIs relisent `sync_status` / `sync_conflicts` / `sync_pending_hosts` /
`synced_snippets_unconfirmed`.

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
   avant confirmation. L'avertissement de récupération vient **avant** l'étape irréversible, dans l'étape elle-même (revue S1c,
   Q1) : case « I saved it. A lost passphrase cannot be recovered unless a device remembers the key. » qui conditionne
   « Create » (passphrase tapée ou générée) et, sur la revue, « Join » ; réussite = « Sync is on ».
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

**Livré en S1a (93 tests unitaires et de propriétés dans `sync::`, dont ceux des correctifs de la revue S1a)** :
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
  incohérent refusé ; réplica sous sa propre clé ; génération de clés au-delà de celle du bundle refusée.
- `clock` / `model` : frères concurrents, valeurs égales fusionnées, pierre tombale contre édition = conflit, Union, résolutions
  vérifiées, collecte seulement stable et jamais d'un conflit, `gc_tombstone_not_resurrected_by_new_joiner_on_rolled_back_folder`,
  `restored_data_dir_never_reuses_a_dot`, point divergent signalé, liste blanche des réglages, validation des écritures,
  `synced_identity_file_rejects_parent_and_unc`, `synced_identity_file_rejects_expansion_chars_and_trailing_dots`,
  `merge_that_would_break_invariants_is_refused_and_leaves_state_intact`, fusion trop volumineuse refusée,
  `inflated_third_party_context_is_deferred_not_merged`, `unknown_kind_edited_twice_relays_only_the_latest_and_stays_readable`,
  champ plus récent sur un genre connu retenu et relayé intact, affirmation d'un tiers sans restauration, invariants
  structurels ; **proptest** : commutativité,
  associativité, idempotence, convergence quel que soit l'ordre (avec doublons), aucune écriture perdue sauf écrasée, conflit ⇔
  écritures concurrentes de valeurs différentes.
- `guard` : retour arrière et fourche par appareil, `older_bundle_toml_is_refused_after_passphrase_change`, cliquet de format,
  état local restauré, mises à jour retenues, ancienneté sur l'heure locale, `fingerprint_ignores_unauthenticated_formatting`,
  horloges vérifiées et affirmations de contexte.
- `passphrase` : liste EFF (7776 mots, triée, SHA-256 épinglé), générateur (6 mots distincts, uniforme, χ² sur 16 classes),
  rejet de la zone biaisée, seuil de force, `typed_list_words_in_list_order_are_weak`, coût borné sur entrée longue.

**Livré en S1b (36 tests de plus, 129 dans `sync::` et `engine::sync_api`)** :
- `folder` : noms des fichiers d'appareil (primaire, copies de chaque fournisseur, placeholders iCloud, temporaires ignorés),
  racine refusée dans `~/.ssh` ou les dossiers de Portico, aller-retour, plafond de lecture, création sans écrasement,
  pause au-delà de 256 entrées sans rien supprimer, dossier absent jamais recréé,
  `devices_dir_symlink_is_refused_and_nothing_written_outside`, fichiers liés ni lus ni remplacés, racine remplacée par
  un lien refusée.
- `project` : projection des quatre fichiers, `updates.check` et `copy_on_select` jamais projetés, `hosts.conf` non géré
  suspend les seuls hôtes (et garde leur dernière projection), édition à la main, suppression et renommage détectés,
  snippet invalide (ESC) ni publié ni supprimé, chemins d'identité relatifs à `~`.
- `state` : `sync.toml` sans secret, confirmations, verrou inter-processus.
- `engine::sync_api` (deux moteurs ou plus, un dossier partagé simulé) : convergence avec conflit hors ligne résolu par
  Union ; `sync_destination_change_held_until_confirmed` (refus, nouvelle destination, confirmation) ;
  `join_prefer_remote_does_not_confirm_redirects` ; `sync_apply_refuses_hostname_like_alias` (nom d'hôte, alias de
  `~/.ssh/config`, « ignorer sur cet appareil ») ; `quarantined_or_held_record_never_becomes_tombstone` ;
  `synced_snippet_change_requires_confirmation_before_run` ; suppressions propagées par pierres tombales ; dossier de
  données restauré → nouvel id sans perte ; retour arrière d'un fichier, fichier tronqué « en attente », copie en conflit
  utilisée, nettoyage des seules copies propres vérifiées ; dossier absent, lecture seule ; deux processus sur un appareil
  (verrou, sans fourche) ; désactivation (clé oubliée, données locales gardées), « ne pas mémoriser » puis déverrouillage ;
  jeton de rattachement à usage unique et expirant ; création refusée (passphrase faible, bundle existant, dossier
  privé) ; `hosts.conf` non géré ; changement et réinitialisation de passphrase alertant les autres appareils ; tâche de
  fond qui publie après une modification locale ; liste blanche des réglages ; enregistrements d'un Portico plus récent
  retenus et jamais appliqués ; contexte de tiers gonflé différé et signalé.

**À venir** : injection de crash entre chaque étape de §5.1 et fourche par clonage de profil complet (couverts en partie :
réplica persisté avant publication, dossier restauré) ; visibilité retardée et réordonnancement du fournisseur simulé ;
conflit concurrent de `bundle.toml` entre deux appareils (politique livrée, scénario à écrire) ; repérage S0 des
fournisseurs réels. S2 —
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
| **S1b** ✅ | Dossier (`FolderIo`, racine sûre), projection, application validée (quarantaine, masquage, confirmation des destinations), état local, tâche de fond, statut et événements, API create/join/unlock/resolve/change/reset/disable | Deux moteurs sur un dossier partagé convergent, conflit hors ligne compris ; scénarios du `SimulatedProvider` verts |
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
| `keyset_generation` aussi lié à l'AAD des instantanés et du réplica | Demandé par la liste de liaison de S1a ; un conteneur au-delà de la génération courante du bundle est refusé à l'ouverture (revue S1a, constat 6) |
| Registres stockés par point (pas de contexte par registre), avec un seul contexte global | Le contexte global (F6) rend le contexte par registre redondant ; le regroupement des valeurs égales garde tous les points, ce qui préserve les lois de fusion (une fusion qui jetait des points rendait la fusion non associative) |
| ~~Passage opaque (union d'octets plafonnée à 256 Kio, « purger »)~~ → **registres génériques** (§4.10) : tout enregistrement non lu exactement est retenu, fusionné et relayé causalement ; plus de plafond propre (les plafonds d'enregistrements et de taille s'appliquent), plus d'action « purger » | Revue S1a, constat 4 : l'union d'octets n'avait pas de causalité (versions remplacées accumulées, clé en double fatale chez une version plus récente). « Purger » un registre couvert par notre contexte le supprimerait chez les autres (F6) : action retirée |
| Alias gardés tels qu'écrits dans les clés | Les collisions à la casse près se contrôlent à l'application avec les règles existantes de `managed_hosts` (S1b) |
| Estimation : modèle diceware (`mots distincts × log2 7776`) **pour une passphrase générée seulement** (`GeneratedPassphrase`, constructible par le seul générateur, mots distincts) ; passphrase tapée : `zxcvbn`, et si tous les mots sont de la liste, le minimum avec un modèle de séquence (premier mot `log2 7776`, puis `log2(1 + distance dans la liste)`) ; minimum de 60 bits pour les deux | Revue S1a, constat 5 : le crédit diceware s'appliquait à toute suite de mots de la liste (« abacus abdomen abdominal abide abiding », les cinq premiers, passait) ; `zxcvbn` plafonne vers 64 bits et accepte aussi ces mots |
| Empreintes des marques hautes sur le contenu authentifié, pas sur les octets du fichier | Revue S1a, constat 1 (§2.7) |
| Fusion transactionnelle, contextes de tiers différés, restauration sur preuve propre seulement | Revue S1a, constats 2 et 3 (§2.7, §4.1) |
| Conteneur refusé au-delà de la `keyset_generation` du bundle | Revue S1a, constat 6 (la politique « ≤ génération courante » prévue pour S1b est faite dans `container::open`) |
| Code `sync_snapshot_invalid` pour un instantané non authentifié, distinct de `sync_corrupt` | Un fichier en cours d'envoi ou tronqué (E2) doit pouvoir être traité comme « en attente » par S1b |
| Plafond KDF 512 Mio / t=6 (au lieu de 1 Gio / t=10) | F13 |
| Roster limité à 64 appareils | Tenir dans le plafond de 64 Kio de `bundle.toml` avec des noms de 64 caractères |

### Ce que S1b attend de S1a
`bundle::{create, open, open_with_key, rewrap, change_passphrase, with_roster}` (nouvelle passphrase en `&NewPassphrase`),
`OpenedBundle { header, roster, data_key, keys, fingerprint }` ; `container::{seal_snapshot, open_snapshot, seal_replica,
open_replica, Binding, Expected, Sealed, SnapshotContent, ReplicaContent}` (`Expected` porte `max_keyset_generation` ; `seal_*`
rend `Sealed { text, fingerprint }`, l'empreinte à garder pour `check_own_file`) ; `model::{SyncState, RecordKey, RecordValue,
WireKey, WireValue, RegisterView, Projection, HeldRecord, Conflict, TypedSibling, validate_record, Resolution, MergeReport}`
(`merge(&other, &source)` faillible, `project()` à appliquer, `held` jamais appliqué ni supprimé) ; `clock::{DeviceId,
DeviceName, LocalClock, VersionVector}` (`observe_own_context` / `observe_own_counter` sur les fichiers propres seulement,
`check_claim` pour les tiers) ; `guard::{HighWater, check_own_file, withheld, check_context_claims, ClaimCheck, ContextClaim,
STALE_HORIZON_SECS}` (`observe_snapshot(device, seq, clock, empreinte, maintenant)`, `verified_clocks()` complété par le
compteur local et les horloges des instantanés tout juste vérifiés : lire **tous** les instantanés d'une passe avant de juger
les contextes) ; `passphrase::{generate_passphrase, assess_passphrase, NewPassphrase, GeneratedPassphrase}` (publics, pour les
jauges des UIs ; `assess_passphrase` juge une saisie, `NewPassphrase::strength` ce que le cœur imposera). Les `#[allow(dead_code)]` de
`sync/mod.rs` tombent avec S1b. **Fait en S1b** : tout ce qui précède est utilisé ; les `allow(dead_code)` sont retirés (les
seuls restants, ciblés et commentés : les sous-clés `secret` et `compare` de S2 ; quelques aides de S1a réservées aux tests
sont en `#[cfg(test)]`).

---

### Écarts de l'implémentation S1b

| Écart | Raison |
|---|---|
| Modules `runner.rs` (passe et opérations), `api.rs` (types publics) et `engine/sync_api.rs` (méthodes du moteur, thread, tâche) au lieu de `state.rs` / `task.rs` seuls | `SyncCore` tient l'état en mémoire (clé, rapport, jetons) sur un seul thread ; les méthodes du moteur doivent voir ses champs privés |
| Le thread `portico-sync` exécute la **passe entière** (dossier, fichiers locaux, trousseau), pas seulement les E/S du dossier ; délai de 60 s par opération | Une passe est une séquence ordonnée sous `sync.lock` : la couper entre threads ajouterait des états intermédiaires. Argon2 reste sur `spawn_blocking` |
| Fichiers temporaires `devices/.<id>.snap.tmp-<aléa>` au lieu de `~<id>.snap.tmp` | Même garantie (`O_EXCL`, `0600` / DACL, `fsync`, `rename`) ; le listage ignore tout nom commençant par `.`. Unix (revue S1b, 9) : écriture ancrée sur descripteurs (`portico-sync/` puis `devices/` ouverts `O_DIRECTORY | O_NOFOLLOW`, `openat(O_CREAT | O_EXCL | O_NOFOLLOW)`, `renameat` / `linkat` dans le même descripteur) |
| Publication : passe 5 s après la dernière modification locale (au plus `poll` après la première), sans espacement minimal de 30 s entre publications ; passe finale à l'arrêt seulement si une modification locale attend | Le délai de 5 s regroupe déjà les rafales ; un état inchangé n'est jamais republié (empreinte de l'état publié) |
| Passe de fond **sautée** quand l'autre processus tient `sync.lock` ; une commande de l'utilisateur (`sync_now`, résolutions) **attend** le verrou | §4.9 ; une commande ne doit pas répondre avec un statut périmé |
| Renommage détecté à la projection (un hôte disparaît, un autre de même destination apparaît) → `renamed_from` ; le nouvel alias reste un **nouvel hôte** qui demande confirmation ailleurs | F1 strict : un nouvel alias est un nouvel hôte |
| Suppression reçue d'un hôte : retiré de `hosts.conf` (refusée tant qu'un autre hôte rebondit par lui, `sync_host_in_use`), **métadonnées et mot de passe mémorisé laissés** | Les métadonnées ont leurs propres pierres tombales ; un appareil compromis ne doit pas pouvoir effacer des secrets locaux |
| « Refuser » une destination la garde hors de `hosts.conf` sur cet appareil (empreinte locale), sans écriture propagée ; une autre destination redemande | Le refus est une décision locale ; « garder le mien partout » = modifier l'hôte localement (écriture ordinaire) |
| Collecte des pierres tombales **après** l'application, en gardant celles dont la suppression n'est pas encore appliquée localement, et celles d'un genre suspendu ou d'une clé non synchronisable (revue S1b, 8) | Sinon une pierre tombale déjà stable était collectée avant d'être appliquée et l'hôte restait (trouvé par `deletions_propagate_as_tombstones`) |
| Suppressions reçues **différées** (`LocalReplica::deferred_deletions`) : une suppression que cet appareil ne peut pas appliquer (fichier suspendu, clé non synchronisable) est retenue localement et appliquée quand le fichier redevient lisible, sauf si la clé a reçu une valeur entre-temps | Le contexte stable des autres appareils dit seulement que cet appareil a **vu** la pierre tombale : ils la collectent, et la fusion la retire ici aussi. Sans cette trace, l'hôte restait pour toujours sur l'appareil suspendu (trouvé par `tombstone_kept_while_hosts_file_unmanaged`) |
| Une clé locale non synchronisable (`unsynced` : `IdentityFile` hors du dossier personnel, valeur refusée par la sync) n'est jamais écrasée ni supprimée par une valeur reçue : issue `sync_local_host_unsynced` (hôtes) ou `sync_local_unsynced` (métadonnées, snippets, réglages), ignorable | Revue S1b, 5 : l'hôte local était remplacé et présenté comme « nouveau » |
| Confirmations d'hôte liées au contenu montré et **à usage unique** : `(alias, destination_hash)` ; consommée quand la destination est écrite ou qu'une modification locale de l'hôte est projetée ; un refus tombe aussi à la modification locale | Revue S1b, 1 et 7 |
| `confirm.toml` est une **barrière d'exécution**, pas un état de sync : écrite **avant** le snippet reçu (échec → rien d'écrit), jamais effacée par « désactiver », une entrée ne part que par confirmation de ce contenu exact ou suppression du snippet ; une copie « garder les deux » d'un côté reçu y est inscrite aussi | Revue S1b, 2, 3 et 4 |
| Clé de données inscrite au trousseau avant la fin de create / join : supprimée si la mise en place échoue avant `sync.toml`. Une entrée orpheline laissée par un crash à cet instant n'est **pas** retrouvable par « désactiver » (qui lit l'id du bundle dans `sync.toml`) : elle ne déchiffre que ce bundle, déjà déchiffrable par la passphrase ; S2 (« oublier un appareil ») pourra la lister | Revue S1b, 12 |
| Jetons de rattachement expirés (et leur DK) purgés au début de **chaque** opération du thread `portico-sync` | Revue S1b, 11 |
| `bundle.toml` réécrit (copie gagnante, roster divergent) seulement **après** le contrôle du fichier propre de l'appareil (§5.1) | Revue S1b, 14 |
| Fichier propre en avance (restauration) : ce que l'ancien id a publié passe par `check_context_claims` avant fusion (différé sinon, puis fusionné plus tard comme le fichier d'un autre appareil) ; un `MergeRefused` est une alerte nommant l'ancien id | Revue S1b, 10 |
| Fichier d'appareil non authentifié **ou** malformé = « en attente » (`PartialSync`), sans compteur « stable sur deux passes » | Il ne supprime jamais rien (fusion par état) ; un fichier qui reste illisible reste visible comme attente |
| Alerte bloquante de réinitialisation (#8) : toute `keyset_generation` plus haute que celle connue ou faite par cet appareil donne `PassphraseChanged`, à acquitter (`sync_acknowledge_alerts`) | Le roster scellé (`deny_unknown_fields`) ne peut pas porter de marqueur « réinitialisée » sans nouveau format ; l'alerte couvre changement et réinitialisation, ce qui est plus sûr |
| `bundle.toml` divergent (même triplet, autre contenu) : gagnant = plus haut (époque, génération, révision) parmi `bundle.toml` et les copies du fournisseur, égalité → plus petite empreinte ; rosters réunis et `bundle.toml` réécrit (révision + 1) ; copies jamais supprimées ; l'appareil se rajoute au roster s'il en manque | Convergence sans perte d'appareil quand deux appareils rejoignent en même temps |
| « Recréer le bundle depuis cet appareil » (§5.5) non livré : `FolderUnavailable { BundleMissing }`, aucune écriture | Demande la passphrase et un écran d'avertissement (S1c) |
| `Resolution::Value` non livré | Modifier l'enregistrement localement est déjà une résolution (une écriture locale couvre tous les frères) |
| Réplica illisible (non authentifié ou malformé) → nouveau rattachement sous un nouvel id avec un état vide (alerte `Restored`) | Aucun point de l'ancien id ne peut être réutilisé ; l'état revient des autres appareils |
| Fichier propre forké (même séquence, autre contenu) traité comme une restauration : nouvel id | Même remède que §2.7 « si c'est cet appareil » |
| Appareil retenu (`guard::withheld`) signalé tout de suite comme avertissement `sync_pending`, sans délai de 24 h | Avertissement non bloquant ; l'ancienneté reste mesurée sur l'heure locale (F17) |
| Windows : points d'analyse refusés par `FileType::is_symlink` (lien, jonction) **et** comparaison du chemin canonique ; placeholders cloud acceptés ; lecture sans `FILE_FLAG_OPEN_REPARSE_POINT` | Ce drapeau empêcherait l'hydratation d'un fichier OneDrive ; la fenêtre résiduelle entre contrôle et `rename` (Windows seulement depuis la revue S1b) est documentée dans `folder.rs` (un processus qui écrit dans le dossier peut déjà tout y remplacer, et ce qui est écrit est chiffré) |
| F8 appliqué par le cœur via `Engine::snippet_input_for`, **seule** source publique des octets d'un snippet (`snippets::snippet_input` est `pub(crate)`) ; la GUI et la TUI y passent (revue S1b, 6) | La TUI affiche le refus `sync_snippet_unconfirmed` (message et indice) ; l'écran de confirmation vient avec S1c |
| `SyncStatus::Error { summary }` ajouté ; `sync_status` rend `SyncOverview` (statut, dossier, appareil, mémorisation) et `Syncing` avant la première passe du processus | Les UIs ont besoin du dossier et d'un état d'échec générique |

### Ce que S1c attend de S1b
- **Démarrage** : `Engine::start_sync()` au lancement (puis rien : la tâche fait une passe immédiate, toutes les 30 s et 5 s
  après chaque modification locale) ; `sync_status()` pour la barre ; s'abonner à `EngineEvent::Sync { status }`,
  `SyncConflicts { count }`, `SyncPendingChanged` (recharger les hôtes reçus et les snippets à confirmer, et **fermer tout
  écran de confirmation montrant l'ancien contenu**), `SnippetsChanged` (recharger), `SettingsChanged { settings }` (thème
  et keymaps en direct ; la GUI valide les keymaps reçues comme la TUI, F18), `HostsChanged` / `HostMeta`. Après `Lagged` :
  `sync_status()`, `sync_conflicts()`, `sync_pending_hosts()` et `synced_snippets_unconfirmed()`.
- **Délai dépassé** : une opération qui rend `sync_folder_timeout` (60 s) peut **se terminer quand même** en arrière-plan
  sur le thread `portico-sync`. L'UI ne suppose ni succès ni échec : elle relit `sync_status()`, `sync_pending_hosts()`,
  `synced_snippets_unconfirmed()` et `sync_conflicts()` (et attend les événements) avant de proposer de recommencer.
- **Assistant** : `sync_probe_folder` (Usable | ExistingBundle | Incomplete | Missing | Refused), `sync_generate_passphrase`
  (montrée une fois), jauge par `assess_passphrase` / `NewPassphrase::strength`, `sync_create(SyncCreate { folder,
  passphrase: NewPassphrase, device_name, remember_key: true par défaut })` ; rejoindre : `sync_join_preview` (rien
  n'est écrit ; compte hôtes, métadonnées, snippets, réglages, conflits, « nécessite plus récent ») puis
  `sync_join_commit(token, ReviewEach | PreferRemote | PreferLocal)` — le jeton expire (10 min) et ne sert qu'une fois.
- **Barre / panneau** : `SyncStatus` (glyphe + libellé), `sync_devices` (`label` = nom + préfixe d'id, F16), `sync_now`,
  `sync_change_passphrase` (dire qu'il ne protège pas d'une passphrase déjà fuitée, F2), `sync_reset_passphrase`
  (confirmation supplémentaire), `sync_rename_device`, `sync_set_options`, `sync_disable(DisableOptions { forget_key: true,
  remove_my_file })`, `sync_unlock` quand `Locked`, `sync_acknowledge_alerts` pour `PassphraseChanged`.
- **Conflits** : `sync_conflicts` (côtés avec appareils, `local`, valeur) et `sync_resolve(id, Pick { index } | Union |
  KeepBoth { keep })` — Union présélectionnée pour les tags, KeepBoth pour les snippets.
- **Issues** : `sync_issues` (code, message, indice ; `warning`, `skippable`) et `sync_skip_on_this_device(key)`.
- **Hôtes reçus (F1)** : `sync_pending_hosts` (nouveau ou non, destination actuelle et reçue, appareils, `renamed_from`,
  `destination_hash`), `sync_confirm_hosts` / `sync_reject_hosts` avec `(alias, destination_hash)` **tel que montré** :
  `sync_not_pending` si la destination a changé depuis (recharger, remontrer). Une confirmation sert une fois. Badge dans
  la liste des hôtes ; aucun choix groupé ne vaut confirmation. `sync_local_host_unsynced` : l'hôte local de même alias
  n'est pas synchronisable et reste tel quel (issue ignorable).
- **Snippets (F8)** : badge et écran via `synced_snippets_unconfirmed` (`UnconfirmedSnippet { id, snippet: { name,
  command, description, run }, hash }`), commande complète puis `confirm_synced_snippet(id, hash)` (`sync_not_pending`
  si le contenu a changé). Les snippets restent bloqués après « désactiver ». La TUI envoie déjà par `snippet_input_for`
  (refus affiché en notice avec l'indice) ; S1c ajoute l'écran de revue et de confirmation.
- GUI : DTO dans `src/lib/api.ts` (types de `crate::sync::api`), commandes Tauri, capability `dialog:allow-open` (dossiers
  seulement). `docs/KEYBINDINGS.md`, palette « Sync… ». Contrat `CLAUDE.md` §2/§4 (module `sync/`) et `SECURITY.md` à la
  clôture.

### Livraison S1c (2026-10-27)

Les deux UIs couvrent tous les parcours de §6 et ce que la liste ci-dessus attend.

- **TUI** (`apps/portico-tui/src/sync_panel.rs`, `sync_wizard.rs`, `app_sync.rs`, `ui/sync.rs`) : `S` sur la liste des
  hôtes et `Ctrl-Space y` partout (la TUI n'a pas de palette : c'est son « Sync… ») ; panneau à cinq onglets, ceux de
  la GUI dans le même ordre (Overview, Conflicts, Received, Issues, Devices ; `1`-`5` identiques), assistant, élément de barre de statut (glyphe `⇅`
  monochrome et libellé, comptes en couleurs de statut), badge `⇅` et ligne « changed by <appareil> » sur l'hôte,
  bandeau « Sync » sous les détails, badge des snippets non confirmés. `start_sync` au lancement ; événements
  appliqués (`SettingsChanged` passe par les mêmes contrôles que `settings.toml`, F18) ; relecture complète après
  `Lagged`, après `sync_folder_timeout` (dit « peut encore aboutir ») et après chaque opération.
- **GUI** (`apps/portico-gui/src-tauri/src/sync.rs`, `src/lib/state/sync.svelte.ts`, `Sync*.svelte`) : « Sync… » dans
  la palette (pas de raccourci global par défaut), élément de statut cliquable ; le sélecteur de dossier tourne côté
  Rust (aucune permission de dialogue dans la webview, capabilities inchangées) ; une passphrase générée reste côté
  Rust sous un jeton à usage unique et n'est montrée qu'une fois ; un scénario E2E bureau couvre la mise en place et
  la désactivation ; mode démo `?sync=…` et `?syncfolder=…` (`docs/TESTING.md`).
- **F1 / F8** : chaque décision porte sur l'empreinte affichée (destination, commande, et depuis la revue S1c les
  côtés d'un conflit : `SyncConflict::sides_hash`) ; sélection explicite élément par élément, chaque destination
  visible, rien de présélectionné, aucun « tout sélectionner » (la GUI offre « Confirm selected » sur des cases
  cochées une à une ; la TUI décide un élément à la fois) ; `sync_not_pending` recharge et demande de revoir. TUI :
  touches ignorées 400 ms après un changement de sélection ou de contenu (y compris de nouveaux côtés d'un conflit),
  et tant que le rechargement d'un `SyncPendingChanged` n'est pas revenu ; un élément reçu ne se confirme qu'une fois
  sa dernière ligne affichée, comme la GUI (revue S1c, A1). L'envoi d'un snippet refusé par
  `sync_snippet_unconfirmed` ouvre sa revue (commande entière), puis revient aux snippets.
- **Correctifs des revues S1c** (sécurité A1–A6, conception B1–B16, questions Q1–Q2) : voir §12, « Revues de
  l'implémentation S1c ». Le cœur ajoute `SyncConflict::sides_hash` et `sync_resolve(id, resolution, sides_hash)`
  (`resolve_refuses_a_stale_sides_hash`) et met les identifiants de ses messages entre accents graves.

| Écart S1c | Raison |
|---|---|
| ~~Avertissement de récupération : TUI avant l'étape qui écrit, GUI en écran final~~ | Aligné par la revue S1c (Q1) : dans les deux UIs, case « I saved it… » dans l'étape qui écrit |
| ~~Raccourci : TUI `S` / `Ctrl-Space y` ; GUI palette seulement~~ | Aligné par la revue S1c (Q2) : GUI `⇧S` dans la liste et `⌘⇧Y` / `Ctrl-Shift-Y` |
| Presse-papiers TUI vidé 30 s après la copie d'une passphrase sans vérifier ce qu'il contient | Un terminal ne permet pas de lire son presse-papiers de façon fiable (OSC 52 en lecture rarement permis) : la TUI vide sans condition, sauf si Portico a copié autre chose entre-temps |
| « Recréer le bundle depuis cet appareil » (§5.5) proposé nulle part | Le cœur ne le fournit pas encore (`FolderUnavailable { BundleMissing }` est expliqué, sans action) |
| Acquittement proposé pour `PassphraseChanged` seulement | C'est la seule alerte que `sync_acknowledge_alerts` efface ; les autres sont expliquées avec leur indice |

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
| F1 | ÉLEVÉ | Hôtes reçus soumis aux contrôles de masquage de l'import (quarantaine `sync_alias_shadowed`) ; nouvel hôte et changement de HostName/Port/User/ProxyJump/IdentityFile retenus jusqu'à confirmation sur l'appareil ; PreferRemote/PreferLocal ne confirment jamais | ✅ S1b ; ✅ UI S1c |
| F2 | ÉLEVÉ | En S1, « abandonner ce bundle et en créer un nouveau » est le seul remède ; l'UI de changement dit qu'il ne protège pas d'une passphrase déjà fuitée ; rotation + oubli d'appareil en S2 avant les mots de passe ; recommander de changer les mots de passe serveurs déjà synchronisés après rotation | limite testée en S1a ; ✅ UI S1c ; rotation S2 |
| F3 | ÉLEVÉ | Jamais de mot de passe non lié ; liés seulement, envoyés par B seulement si sa clé vérifiée = l'empreinte liée ; mention en lecture seule à l'invite TOFU | S2 (genre `Secret` réservé en S1a, jamais écrit) |
| F4 | MOYEN | Marque haute locale du bundle, refus des rétrogradations ; keyset_generation dans l'AAD de l'emplacement ; cliquet de format | ✅ S1a |
| F5 | MOYEN | Lire son propre fichier au démarrage et à chaque passe ; s'il est en avance → nouvel id ; horloge, séquence, génération dans le réplica scellé, un seul fichier atomique | ✅ S1a (primitives) ; ✅ S1b (câblage) |
| F6 | MOYEN | Contexte causal global jamais collecté ; un enregistrement absent couvert par le contexte est supprimé, pas ressuscité | ✅ S1a |
| F7 | MOYEN | La projection garde l'état non appliqué des clés retenues / en quarantaine / masquées / exclues ; seule la suppression d'une valeur réellement écrite donne une pierre tombale | ✅ S1b |
| F8 | MOYEN | Snippet modifié par la sync : badge, corps complet et confirmation au premier lancement | ✅ S1b (refus par le cœur, GUI et TUI branchées, barrière liée au contenu) ; ✅ écran de confirmation S1c |
| F9 | MOYEN | Racine canonisée ; contrôle `symlink_metadata` à chaque passe ; points d'analyse Windows ; exclusion de `~/.ssh`, config, données | ✅ S1b |
| F10 | MOYEN | IdentityFile : composantes normales seulement sous `~` | ✅ S1a |
| F11 | MOYEN | Détection de changement de secret par compteur de génération d'index ; HMAC(K_cmp, compte ‖ secret) dans l'enregistrement scellé | S2 (K_cmp dérivée en S1a) |
| F12 | MOYEN | « Inclure les mots de passe » est un réglage du bundle, présenté comme tel | S2 |
| F13 | FAIBLE | Bornes KDF propres (défaut 256 Mio t=3 p=1, plancher 128 Mio, plafond 512 Mio t=6 p=4) ; `try_reserve` + `hash_password_into_with_memory` ; (paramètres, sel) retenus localement | ✅ S1a |
| F14 | FAIBLE | Sous-clés K_roster et K_local distinctes ; MAC sur encodage canonique préfixé ; comparaison en temps constant ; aucune écriture conditionnée au déballage avant vérification du MAC ; jeton de rattachement à usage unique avec expiration, DK en `Zeroizing` ; DK mémorisée en base64 `Zeroizing`, élément macOS non synchronisable ; « Désactiver » oublie la DK par défaut | ✅ S1a (crypto) ; ✅ S1b |
| F15 | FAIBLE | ≤ 8 candidats par appareil, budget d'octets par passe, pause sans suppression au-delà de la limite de listage ; ~~passage opaque plafonné (256 Kio) + « purger »~~ remplacé par les registres génériques (§4.10), bornés par les plafonds d'enregistrements et de taille | ✅ S1a (registres) ; ✅ S1b (dossier) |
| F16 | FAIBLE | Nom d'appareil validé (pas de contrôle / bidi / invisible), toujours affiché avec le préfixe d'id | ✅ S1a |
| F17 | FAIBLE | Ancienneté mesurée sur l'heure locale d'observation de la dernière nouvelle séquence, jamais `published_at` | ✅ S1a |
| F18 | FAIBLE | `copy_on_select` non synchronisé ; la GUI valide les keymaps reçues comme la TUI ; force de passphrase imposée dans le cœur (`sync_passphrase_weak`) à la création, au changement et à la réinitialisation | ✅ S1a (cœur) ; ✅ GUI S1c |

### Revues de l'implémentation S1c (2026-10-27)

**Sécurité** (agent-security, sur `8761671..03efaaf`) : aucun constat CRITIQUE ni ÉLEVÉ. Tous corrigés.

| # | Gravité | Constat | Correctif | État |
|---|---|---|---|---|
| A1 | MOYEN | GUI : la commande d'un snippet reçu pouvait cacher une charge sous un pli de défilement invisible (F8) | GUI : « N lines · M characters », numéros de ligne, confirmation inactive tant que la fin n'a pas défilé, fondu « ↓ more ». TUI (parité) : `y` refusé tant que la dernière ligne de l'élément reçu n'a pas été affichée (« PgDn read to the end ») | ✅ |
| A2 | FAIBLE | TUI : les tabulations d'une commande reçue étaient retirées à l'affichage mais tapées | Tabulation affichée en `→` discret (revue des snippets, vues de conflit) ; GUI de même | ✅ |
| A3 | FAIBLE | GUI : la passphrase générée restait côté Rust après la fermeture du dialogue | `sync_forget_generated` à la fermeture, expiration 10 min, jeton aléatoire | ✅ |
| A4 | FAIBLE | La passphrase générée restait dans le presse-papiers | Vidé après 30 s (TUI : OSC 52 vide, séquence en `Zeroizing`, sauf copie plus récente ; GUI : voir ci-dessous) ; note « Copied — cleared from the clipboard in 30 s » | ✅ |
| A5 | FAIBLE | Résolution d'un conflit non liée au contenu montré | `SyncConflict::sides_hash`, `sync_resolve(id, resolution, sides_hash)` → `sync_not_pending` si les côtés ont changé ; TUI : `↵` réarmé 400 ms à de nouveaux côtés ; GUI : choix réinitialisés | ✅ |
| A6 | FAIBLE | La doc disait « aucune confirmation groupée » alors que la GUI a « Confirm selected » | Décision : comportement GUI conservé, doc reformulée (sélection explicite élément par élément, rien de présélectionné, aucun « tout sélectionner ») | ✅ |

Côté GUI, en pratique (`8bb6ffd`, `eaf150c`) :

- **Presse-papiers (A4)** : `apps/portico-gui/src/lib/secretclip.ts` ne garde qu'un SHA-256 de la passphrase copiée,
  jamais son texte. Après 30 s, il relit le presse-papiers (`readText`, la permission du collage du terminal) et le
  vide s'il contient toujours cette passphrase. Si la lecture est refusée, il le vide quand même, mais seulement si
  cette copie est encore la dernière faite par ce module (une copie plus récente a son propre minuteur) : vider un
  presse-papiers rempli entre-temps est un moindre mal que d'y laisser une passphrase. Le minuteur survit à la
  fermeture du dialogue.
- **Revue d'un snippet reçu (A1)** : « N lines · M characters », numéros de ligne, tabulations et caractères de
  contrôle visibles ; « Confirm » (bouton et touche) inactif tant que la fin de la commande n'a pas été vue (défilement
  jusqu'en bas, ou tout de suite si elle tient), remis à zéro quand la vue change ; « ↓ more » tant qu'elle n'est pas
  au bout. La revue nomme l'appareil (« Changed by studio-mac (3f9c0ae1). », `UnconfirmedSnippet::devices`, B14).
- **Passphrase générée (A3)** : gardée côté Rust sous un jeton aléatoire de 128 bits, à usage unique, expirée après
  10 min ; `sync_forget_generated` l'efface dès que le dialogue se ferme ou que l'on tape la sienne.
- **Q1** : case « I saved it. A lost passphrase cannot be recovered unless a device remembers the key. » sur l'étape
  de création (exigée pour « Create ») et sur la revue de rattachement (exigée pour « Join ») ; plus d'écran final.
- **Q2** : `app.sync` = `Mod+Shift+Y` partout (`⌘⇧Y` / `Ctrl-Shift-Y`, l'équivalent de `Ctrl-Space y`) ;
  `hosts.sync` = `Shift+S` dans la liste des hôtes (comme `S` en TUI) ; affichés dans la palette et le titre de
  l'élément de statut.

`UnconfirmedSnippet::devices` (B14, cœur) : labels des appareils qui ont écrit le contenu retenu, lus dans le registre
à chaque passe (cet appareil exclu) ; vide tant que la synchronisation est verrouillée ou désactivée depuis le
lancement (les deux UIs disent alors « another device »).

**Conception** (agent-design-review, verdict « presque ») : tout appliqué. Côté TUI : B2 (identifiants entre accents
graves dans les messages du cœur ; `sentence()` ne met jamais de majuscule à un mot qui commence par un accent grave et
les retire à l'affichage : « legacy is already defined… »), B5 (onglets Overview · Conflicts · Received · Issues ·
Devices dans les deux UIs, libellés « Turn sync off », « Union of all tags », « Keep <appareil>'s », « On this
device », « tags, groups, notes »), B6 (« à revoir » en `status-warning`), B15 (80×24 : la barre de statut dit
« SYNC  ? help » sans répéter les touches du panneau ; colonne de la jauge et de « Remember » alignée sur le texte des
champs ; « this device » marqué du seul point d'accent, « inactive » discret ; détails d'hôte « ⇅ changed by desk ·
S review », l'id à partir de 120 colonnes ; numéro d'étape sans total ; HostName reçu coupé après `.` et `-`, marque
de changement sur la ligne du libellé ; nom d'appareil par défaut `portico-` et quatre chiffres hexadécimaux),
Q1 (avertissement de récupération dans l'étape qui écrit, case « I saved it… » pour créer et rejoindre, réussite
« Sync is on »). Côté GUI : B1, B3, B4, B6–B14, B16, Q2 (voir le commit GUI).

### Revue de sécurité de l'implémentation S1b (agent-security, 2026-10-27)

Aucun constat CRITIQUE ni ÉLEVÉ. Tous corrigés avant S1c.

| # | Gravité | Constat | Correctif | État |
|---|---|---|---|---|
| 1 | MOYEN | Confirmations non liées au contenu montré (F1/F8) : une destination publiée entre l'affichage et le clic était écrite | `PendingHostChange::destination_hash`, `UnconfirmedSnippet::hash` ; `sync_confirm_hosts` / `sync_reject_hosts` / `confirm_synced_snippet` exigent le hash montré (`sync_not_pending` sinon) ; `SyncPendingChanged` au changement de **contenu** | ✅ |
| 2 | MOYEN | « Désactiver » effaçait `confirm.toml` : les snippets reçus non confirmés devenaient exécutables | `confirm.toml` = barrière d'exécution, jamais effacée par « désactiver » ; une entrée part par confirmation ou suppression du snippet | ✅ |
| 3 | MOYEN | Crash ou échec entre `snippets.toml` et `confirm.toml` : contournement de F8, puis republication comme modification locale | Barrière écrite **avant** le snippet (`SnippetStore::save_guarded`), échec → rien d'écrit ; plus d'effacement de barrière à la projection d'une modification locale | ✅ |
| 4 | MOYEN | « Garder les deux » créait la copie du côté reçu comme snippet local sans confirmation | Copie d'un côté sans point de cet appareil inscrite dans `confirm.toml` avant écriture | ✅ |
| 5 | MOYEN | Un hôte local non synchronisable était écrasé et présenté comme « nouveau » | Clé `unsynced` (ou hôte sans valeur synchronisable) jamais écrasée ni supprimée : `sync_local_host_unsynced` ; même garde pour métadonnées, snippets, réglages (`sync_local_unsynced`) | ✅ |
| 6 | MOYEN | La TUI construisait les octets d'un snippet hors du cœur | TUI branchée sur `Engine::snippet_input_for` ; `snippets::snippet_input` passé en `pub(crate)` | ✅ |
| 7 | FAIBLE | Confirmation d'hôte jamais consommée | Retirée à l'écriture de la destination et à toute modification locale projetée de l'hôte (refus aussi) | ✅ |
| 8 | FAIBLE | Pierres tombales collectées pendant la suspension d'un genre | Gardées pour un genre suspendu ou une clé non synchronisable ; suppressions différées pour celles que les autres appareils collectent (§10) | ✅ |
| 9 | FAIBLE | Écritures du dossier par chemin (fenêtre d'échange de répertoire) | Unix : descripteurs `O_DIRECTORY | O_NOFOLLOW`, `openat(O_EXCL | O_NOFOLLOW)`, `renameat` / `linkat` ; Windows : fenêtre résiduelle documentée | ✅ |
| 10 | FAIBLE | Fusion du fichier de l'ancien id sans contrôle des contextes, `MergeRefused` ignoré | `check_context_claims` d'abord (différé sinon) ; `MergeRefused` en alerte nommant l'ancien id | ✅ |
| 11 | FAIBLE | Jetons de rattachement expirés (et DK) gardés jusqu'au prochain aperçu | Purge au début de chaque opération du thread | ✅ |
| 12 | FAIBLE | DK au trousseau avant la réussite de create / join | Supprimée si la mise en place échoue avant `sync.toml` ; orphelin après crash documenté (§10) | ✅ |
| 13 | FAIBLE | `is_set_up` (stat) dans une tâche async | `spawn_blocking` | ✅ |
| 14 | FAIBLE | `bundle.toml` pouvait être réécrit avant le contrôle du fichier propre | Réécriture après ce contrôle (§5.1) | ✅ |
| 15 | FAIBLE | Délai de 60 s : l'opération continue en arrière-plan | Documenté dans « Ce que S1c attend de S1b » : relire statut, hôtes en attente, snippets, conflits | ✅ |

### Revue de sécurité de l'implémentation S1a (agent-security, 2026-10-27)

| # | Gravité | Constat | Correctif | État |
|---|---|---|---|---|
| 1 | ÉLEVÉ | Empreintes des marques hautes sur les octets bruts : un commentaire ou des CRLF ajoutés forgeaient une fourche ou une divergence sans la DK | Empreinte du contenu authentifié (AAD ‖ nonce ‖ chiffré ; entrée du MAC ‖ MAC), rendue par `seal_*` et `open_*` ; `guard::fingerprint(octets)` retiré | ✅ S1a |
| 2 | MOYEN | La fusion n'imposait aucun plafond : un instantané valide d'un appareil compromis empoisonnait l'état local | `SyncState::merge` faillible et transactionnelle (copie, invariants, taille), `sync_merge_refused` nommant la source | ✅ S1a |
| 3 | MOYEN | Contexte causal d'un tiers non borné : suppression silencieuse et nouveau rattachement forcé | `guard::check_context_claims` (instantané différé) ; `dropped_live_without_successor` ; `LocalClock` : restauration sur preuve propre seulement, `sync_third_party_claim` sinon | ✅ S1a (API) ; ✅ S1b (report et alertes) |
| 4 | MOYEN | Passage opaque sans causalité : la compatibilité ascendante promise ne tenait pas | Registres génériques de JSON canonique, décodage typé à la projection, registres retenus jamais appliqués, clés en double fusionnées (§4.10) | ✅ S1a |
| 5 | MOYEN | Estimation diceware contournable par des mots de la liste tapés | `GeneratedPassphrase` (seule à recevoir le crédit diceware), `NewPassphrase::{Generated, Typed}` ; saisie jugée par `zxcvbn` et un modèle de séquence | ✅ S1a |
| 6 | FAIBLE | `HomeRelPath` plus laxiste que `managed_hosts` ; génération de clés non bornée à l'ouverture ; limite Argon2 sous surallocation ; clair JSON réalloué | `check_path_chars` + `windows_name_problem` ; `Expected::max_keyset_generation` ; limite documentée (§2.3) ; `serde_json::to_writer` dans un tampon `Zeroizing` pré-dimensionné | ✅ S1a |
