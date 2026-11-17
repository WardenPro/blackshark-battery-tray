//! Décodage des rapports du dongle BlackShark V3 (USB `1532:057A`).
//!
//! Tout ce qui suit vient de captures sur la collection constructeur `FF14`
//! (rapports d'entrée et de sortie de 64 octets). Razer ne documente rien.
//!
//! ## Trame observée
//!
//! ```text
//! idx  0  1  2  3  4  5  6  7  8  9 10 11 12 13 … 61 62 63
//!     02 02 60 00 00 00 05 00 00 00 21 02 01 45 … 00 02 00
//!     │  │  │  │        │           │  │  │  │     │  └ inutilisé
//!     │  │  │  │        │           │  │  │  └ données (ici : le niveau)
//!     │  │  │  │        │           │  │  └ longueur des données
//!     │  │  │  │        │           │  └ 02 = notification, 01 = réponse
//!     │  │  │  │        │           └ classe de commande (0x21 = niveau)
//!     │  │  │  │        └ longueur du message
//!     │  │  │  └ 0x60 : marque les messages relayés par le dongle
//!     │  │  └ identifiant de commande
//!     │  └ identifiant de commande
//!     └ identifiant de rapport HID
//! ```
//!
//! Le **checksum est en `[62]`** : XOR de tous les octets `0..61`.
//! Pour une notification de niveau, il vaut `0x47 ^ niveau` (vérifié sur
//! toutes les captures : 0x45→0x02, 0x47→0x00, 0x48→0x0F, 0x61→0x26, 0x62→0x25).
//! `[63]` est toujours `0x00`.
//!
//! ## Pourquoi il n'y a pas de polling
//!
//! Envoyer la classe `0x21` avec n'importe quelle combinaison de type, de
//! sous-attribut et de valeur renvoie toujours `21 01 01 FF` : le dongle répond
//! « niveau inconnu » (0xFF). Il n'existe donc aucun moyen de demander le
//! niveau : le casque ne le pousse que lorsqu'il change (voir le README).

/// Page d'usage HID de la collection qui porte les notifications batterie.
pub const USAGE_PAGE: u16 = 0xFF14;

/// Classe de commande « niveau de batterie ».
pub const CLASS_LEVEL: u8 = 0x21;
/// Trame émise spontanément par le casque.
pub const TYPE_EVENT: u8 = 0x02;
/// Trame envoyée par le dongle en réponse à une demande.
pub const TYPE_REPLY: u8 = 0x01;
/// Valeur renvoyée quand le niveau est inconnu (le dongle ne sait pas / ne dit pas).
pub const UNKNOWN: u8 = 0xFF;

const CLS: usize = 10;
const TYP: usize = 11;
const LEN: usize = 12;
const DATA: usize = 13;
const CKSUM: usize = 62;
const FRAME_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
}

/// Vérifie le checksum d'une trame complète (`[62]` = XOR de `0..61`).
fn checksum_ok(buf: &[u8]) -> bool {
    if buf.len() <= CKSUM {
        return true; // trame courte : rien à vérifier
    }
    let xor = buf[..CKSUM].iter().fold(0u8, |a, b| a ^ b);
    xor == buf[CKSUM]
}

/// Données d'un message de la classe niveau, pour le type demandé.
fn level_payload(buf: &[u8], typ: u8) -> Option<&[u8]> {
    if buf.len() <= DATA || buf[0] != 0x02 {
        return None;
    }
    if buf[CLS] != CLASS_LEVEL || buf[TYP] != typ || !checksum_ok(buf) {
        return None;
    }
    let n = buf[LEN] as usize;
    let end = DATA.checked_add(n)?;
    if n == 0 || buf.len() < end {
        return None;
    }
    Some(&buf[DATA..end])
}

/// Notification poussée par le casque : `21 02 01 <niveau>`.
pub fn parse_battery(buf: &[u8]) -> Option<Battery> {
    let percent = *level_payload(buf, TYPE_EVENT)?.first()?;
    (percent <= 100).then_some(Battery { percent })
}

/// Réponse à une demande de niveau : `21 01 01 <niveau|FF>`.
///
/// `Some(None)` = le dongle dit explicitement « je ne connais pas le niveau ».
pub fn parse_reply(buf: &[u8]) -> Option<Option<u8>> {
    let level = *level_payload(buf, TYPE_REPLY)?.first()?;
    if level == UNKNOWN || level > 100 {
        Some(None)
    } else {
        Some(Some(level))
    }
}

/// Trame de demande du niveau. Inoffensive (le dongle l'acquitte en 2–5 ms) mais
/// sans effet sur le niveau lui-même : elle sert de poignée de main à l'ouverture
/// du canal et au clic sur *Rafraîchir*.
pub fn level_query() -> [u8; FRAME_LEN] {
    let mut f = [0u8; FRAME_LEN];
    f[..6].copy_from_slice(&[0x02, 0x02, 0x60, 0x00, 0x00, 0x00]);
    f[6] = 0x05;
    f[CLS] = CLASS_LEVEL;
    f[TYP] = TYPE_REPLY;
    f[LEN] = 0x01;
    f[DATA] = 0x00;
    f[CKSUM] = f[..CKSUM].iter().fold(0u8, |a, b| a ^ b);
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trame telle qu'émise par le dongle (checksum en [62], [63] = 0).
    fn report(level: u8) -> [u8; FRAME_LEN] {
        let mut r = [0u8; FRAME_LEN];
        r[..14].copy_from_slice(&[0x02, 0x02, 0x60, 0, 0, 0, 0x05, 0, 0, 0, 0x21, 0x02, 0x01, level]);
        r[CKSUM] = r[..CKSUM].iter().fold(0, |a, b| a ^ b);
        r
    }

    #[test]
    fn decodes_captured_reports() {
        // (niveau, checksum) réellement capturés sur le dongle.
        for (lvl, crc) in [(0x45, 0x02), (0x47, 0x00), (0x48, 0x0F), (0x61, 0x26), (0x62, 0x25)] {
            let r = report(lvl);
            assert_eq!(r[CKSUM], crc);
            assert_eq!(r[CKSUM + 1], 0x00);
            assert_eq!(parse_battery(&r), Some(Battery { percent: lvl }));
        }
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut r = report(0x45);
        r[CKSUM] ^= 1;
        assert_eq!(parse_battery(&r), None);
    }

    #[test]
    fn rejects_battery_above_100() {
        assert_eq!(parse_battery(&report(101)), None);
        assert_eq!(parse_battery(&report(0xFF)), None);
    }

    #[test]
    fn ignores_replies_and_other_classes() {
        // Une réponse (type 01) n'est pas une notification.
        let mut r = report(0x45);
        r[TYP] = TYPE_REPLY;
        r[CKSUM] = r[..CKSUM].iter().fold(0, |a, b| a ^ b);
        assert_eq!(parse_battery(&r), None);
        // Le flux de télémétrie 0x19 ne doit pas être pris pour la batterie.
        let mut r = report(0x45);
        r[CLS] = 0x19;
        r[CKSUM] = r[..CKSUM].iter().fold(0, |a, b| a ^ b);
        assert_eq!(parse_battery(&r), None);
        assert_eq!(parse_battery(&[0x02; 4]), None);
    }

    #[test]
    fn reply_reports_unknown_level() {
        let mut r = report(0x00);
        r[TYP] = TYPE_REPLY;
        r[DATA] = UNKNOWN;
        r[CKSUM] = r[..CKSUM].iter().fold(0, |a, b| a ^ b);
        assert_eq!(parse_reply(&r), Some(None));
        r[DATA] = 0x5A;
        r[CKSUM] = r[..CKSUM].iter().fold(0, |a, b| a ^ b);
        assert_eq!(parse_reply(&r), Some(Some(0x5A)));
    }

    #[test]
    fn query_frame_is_well_formed() {
        let q = level_query();
        assert_eq!(&q[..14], &[0x02, 0x02, 0x60, 0, 0, 0, 0x05, 0, 0, 0, 0x21, 0x01, 0x01, 0x00]);
        assert_eq!(q[CKSUM], q[..CKSUM].iter().fold(0, |a, b| a ^ b));
        // Le dongle acquitte cette trame (voir README) : elle doit rester de 64 octets.
        assert_eq!(q.len(), 64);
    }
}
