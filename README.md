# blackshark-battery

Icône de barre des tâches Windows affichant le niveau de batterie d'un casque
**Razer BlackShark V3** (dongle USB `1532:057A`). Rust + Win32 natif, sans Synapse,
sans framework graphique : ~300 Ko, un thread qui dort en attente HID.

| Icône | Signification |
|-------|---------------|
| `71` vert / orange / rouge | niveau en % (> 30 / ≤ 30 / ≤ seuil d'alerte) |
| `71` gris | dernière mesure plus ancienne que `stale_s` (âge exact dans l'infobulle) |
| `…` | dongle présent, aucune mesure reçue (casque éteint ou hors de portée) |
| `×` | dongle non détecté |
| `!` | dongle trouvé mais impossible à ouvrir (code d'erreur dans l'infobulle) |

Clic droit : *Rafraîchir* / *Redétecter le dongle* / *Quitter*. L'infobulle indique
toujours depuis combien de temps le niveau affiché a été mesuré. Une notification
Windows prévient quand la batterie passe sous le seuil configuré.

## Comment la batterie est lue

Razer ne publie aucune API pour ça : le SDK Chroma ne gère que l'éclairage. Ce casque
n'expose pas non plus les *feature reports* de 90 octets utilisés par les souris de la
marque. Tout ce qui suit vient de captures sur la collection HID constructeur `FF14`
(rapports de 64 octets) :

```
idx  0  1  2  3  4  5  6  7  8  9 10 11 12 13 … 61 62 63
    02 02 60 00 00 00 05 00 00 00 21 02 01 45 … 00 02 00
     │  │  │        │           │  │  │  │     │  └ inutilisé
     │  │  │        │           │  │  │  └ niveau = 0x45 = 69 %
     │  │  │        │           │  │  └ 02 = notification, 01 = réponse
     │  │  │        │           │  └ classe de commande (0x21 = niveau)
     │  │  │        │           └ longueur du message
     │  │  │        └ 0x60 : messages relayés par le dongle
     │  └ identifiants de commande
     └ identifiant de rapport HID
```

Le **checksum est en `[62]`** : XOR de tous les octets `0..61`, soit `0x47 ^ niveau`
pour une notification (vérifié sur toutes les captures : `0x45→0x02`, `0x47→0x00`,
`0x48→0x0F`, `0x61→0x26`, `0x62→0x25`). `[63]` vaut toujours `0x00`.

### Pourquoi il n'y a pas de polling

En envoyant la classe `0x21` avec toutes les combinaisons de type, de sous-attribut et
de valeur (et en balayant les classes `0x00`→`0xFF`), le dongle répond **toujours**
`21 01 01 FF` : « niveau inconnu ». Aucune commande ne permet donc de demander le
niveau. Le casque le pousse de lui-même, et seulement **quand il change** :

* sur batterie, environ **une notification toutes les 8 à 10 minutes** (mesuré : 1 en
  25 min, puis 1 en 10 min d'écoute) ;
* nettement plus souvent pendant la charge, où le niveau monte vite.

C'est la raison pour laquelle l'icône peut rester plusieurs minutes sur la même valeur,
et pourquoi l'infobulle affiche l'âge de la mesure. L'application ne perd rien : elle
garde le canal ouvert et met l'icône à jour à chaque notification, y compris quand une
autre application (Synapse, THX…) a aussi le dongle ouvert.

Le polling (`poll_ms`) sert uniquement à détecter rapidement le branchement ou le
débranchement du dongle. À l'ouverture du canal, l'outil envoie une demande de niveau
(poignée de main, acquittée en 2 à 5 ms) et retient la réponse si elle contient un
niveau ; la même trame est renvoyée par *Rafraîchir*.

## Compilation

Prérequis : [rustup](https://rustup.rs). La toolchain GNU suffit, Visual Studio n'est pas nécessaire.

```bash
rustup toolchain install stable-x86_64-pc-windows-gnu
```

```bash
cargo +stable-x86_64-pc-windows-gnu build --release
```

L'exécutable produit est `target/release/blackshark-battery.exe`. Si le chemin du
projet est long, ajouter `--target-dir C:\bsb-target` : le *build script* de
`windows_x86_64_gnu` échoue au-delà de ~200 caractères de chemin.

## Installation

1. Copier `blackshark-battery.exe` dans un dossier fixe, par exemple `%LOCALAPPDATA%\BlackSharkBattery\`.
2. (Optionnel) y placer un `blackshark-battery.ini` (voir ci-dessous).
3. Démarrage automatique : `Win+R` → `shell:startup`, puis y créer un raccourci vers l'exe.
4. Pour garder l'icône visible : Paramètres → Personnalisation → Barre des tâches →
   Autres icônes de la barre d'état système → activer *blackshark-battery*.

Lancer l'exécutable depuis l'Explorateur (ou le raccourci), pas depuis un terminal :
sous Windows, un processus lancé par un shell/IDE meurt avec son parent, ce qui donne
l'impression que l'icône « s'est fermée toute seule ».

Synapse peut rester installé : le canal HID est ouvert en mode partagé, et les
notifications sont diffusées à tous les processus qui l'ont ouvert.

### Journal

L'application écrit `%LOCALAPPDATA%\BlackSharkBattery\app.log` (rotation à 200 Ko) :

```
2026-11-17 17:07:18 [43244] démarrage
2026-11-17 17:07:18 [43244] canal 0xFF14 ouvert
```

Elle y note le démarrage, l'ouverture et la perte du canal HID, la fin de session
Windows, l'arrêt par le menu, les paniques et le refus de seconde instance. Sans ce
fichier, une terminaison externe ne laisse aucune trace : `WerSvc` n'est pas toujours
actif, et le Gestionnaire d'événements ne mentionne rien. Si l'icône disparaît encore,
la dernière ligne du journal dit exactement ce qui s'est passé.

## Configuration (`blackshark-battery.ini`, à côté de l'exe)

```ini
poll_ms = 1000        # période de lecture / re-détection du dongle (100 à 600000 ms)
low_battery = 15      # seuil d'alerte (%), 0 = désactivé
stale_s = 900         # au-delà, le niveau affiché est grisé
product_ids = 057A    # PID USB acceptés (hex, séparés par des virgules)
query =               # rapport hex envoyé à chaque cycle (facultatif : le dongle ne
                      # sait pas répondre au niveau, voir plus haut)
```

## Diagnostic

`probe.exe` (construit avec `cargo build --release --bin probe`) :

```bash
probe list            # collections HID Razer présentes
probe listen 120      # affiche les rapports reçus pendant 120 s
probe send FF14 02 02 60 00 00 00 05 00 00 00 21 01 01 00   # écrit puis écoute 1 s
```

Si l'icône reste sur `…` : vérifier avec `probe listen` que des lignes
`FF14 [64] … 21 02 01 xx` arrivent. Si le casque est un autre modèle (PID différent),
ajouter son PID dans `product_ids` — le décodage de la trame reste valable pour les
modèles de la même famille.
