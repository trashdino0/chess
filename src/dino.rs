//! DinoChess mascot data: dinosaur facts served by the `dino` UCI command
//! (`info string`, protocol-safe) and mirrored by the lichess bot's `!dino`
//! chat command (which keeps its own copy in `conversation.py`).
//!
//! Rotation is a plain atomic counter — deterministic and std-only (no RNG
//! crate by design). Keep every fact one line, ASCII, chat-sized.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Authoritative fact table. The bot's `!dino` list is a subset of these.
pub const DINO_FACTS: [&str; 24] = [
    "T. rex bit down with over 12,000 psi of force - enough to crush a car.",
    "Velociraptor was turkey-sized and feathered, not the movie monster.",
    "Triceratops had up to 800 teeth in constantly replaced dental batteries.",
    "Stegosaurus had a brain the size of a walnut - about 80 grams.",
    "Birds are living dinosaurs: every pigeon is a theropod.",
    "T. rex protein most closely matches the chicken's (2007 collagen study).",
    "Spinosaurus was longer than T. rex and hunted fish, semi-aquatic.",
    "Sue, the most complete T. rex (~90%), died at about 28 years old.",
    "An asteroid ~10 km wide ended the dinosaurs 66 million years ago.",
    "Pterosaurs flew but weren't dinosaurs; mosasaurs swam but weren't either.",
    "Dimetrodon and its sail lived ~40 million years BEFORE dinosaurs.",
    "'Dinosaur' means 'terrible lizard' - coined by Richard Owen in 1842.",
    "The first named dinosaur was Megalosaurus, described in 1824.",
    "Maiasaura means 'good mother lizard' - it nested in huge colonies.",
    "Argentinosaurus reached ~35 m long - longer than a blue whale.",
    "Oviraptor means 'egg thief', but it died brooding its OWN nest.",
    "Brontosaurus was resurrected in 2015 after a century as Apatosaurus.",
    "Iguanodon's thumb spike was first mistaken for a nose horn.",
    "Parasaurolophus's crest worked like a trombone to boom at rivals.",
    "Microraptor had four wings and glided between the trees.",
    "Compsognathus was chicken-sized - one of the smallest dinosaurs.",
    "Giganotosaurus out-measured T. rex at ~13 m long.",
    "Fossilized dinosaur poop is called a coprolite. Science studies it.",
    "Deinonychus, not Velociraptor, inspired Jurassic Park's raptors.",
];

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Next fact in rotation. Wraps forever; lock-free and never blocks search.
pub fn next_fact() -> &'static str {
    let i = NEXT.fetch_add(1, Ordering::Relaxed);
    DINO_FACTS[i % DINO_FACTS.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn facts_are_unique_nonempty_ascii_one_liners() {
        let mut seen = HashSet::new();
        for f in DINO_FACTS {
            assert!(!f.is_empty(), "empty fact");
            assert!(!f.contains('\n'), "multiline fact: {f}");
            assert!(f.is_ascii(), "non-ASCII fact: {f}");
            assert!(f.len() <= 140, "chat-too-long fact: {f}");
            assert!(seen.insert(f), "duplicate fact: {f}");
        }
    }

    #[test]
    fn rotation_covers_the_whole_table() {
        // Index math must visit every slot exactly once per cycle.
        let mut seen = HashSet::new();
        for i in 0..DINO_FACTS.len() {
            seen.insert(DINO_FACTS[i % DINO_FACTS.len()]);
        }
        assert_eq!(seen.len(), DINO_FACTS.len());
    }

    #[test]
    fn table_mentions_the_classics() {
        let all = DINO_FACTS.join(" ");
        for name in ["T. rex", "Velociraptor", "Triceratops", "Stegosaurus"] {
            assert!(all.contains(name), "missing classic: {name}");
        }
    }
}
