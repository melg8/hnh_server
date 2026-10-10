//! Animal duel bars and the aggro-leash policy constants.

/// Live combat bars for one animal engaged with a player.
pub struct AnimalFight {
    /// Animal's offence bar toward the player (scaled percentage).
    pub off: i32,
    /// Animal's defence bar against player attacks (scaled percentage).
    pub def: i32,
    /// Battle intensity (animals-and-husbandry.md taming step 2: Jorb's
    /// prerequisite list requires "battle intensity reduced to 0" before
    /// Quell the Beast may fire). Every landed blow (either direction)
    /// raises it; quiet ticks de-escalate it. Same scaled bar as the
    /// offence/defence meters (0..=BAR_FULL).
    pub intensity: i32,
    /// Session 90: consecutive ticks with NO duel activity - no attack
    /// queued by the player and no landed blow (a bite resets it; a
    /// queued attack holds it at zero). A duel idling past
    /// [`IDLE_DUEL_TICKS`] is torn down: a forced duel that neither
    /// side advances (the session-90 live trace: a bear opened a duel
    /// mid-taming, then stood - no bites, no player attack, the fight
    /// living forever under the one-Fightview rule) used to lock the
    /// taming protocol out of every later fight.
    pub idle_ticks: u32,
}

/// Session 90: a completely idle animal duel (no queued attack, no
/// landed blows from either side) ends after this many ticks (30 s at
/// the 10 Hz game loop). Players keep DISENGAGE (300 subtiles) as the
/// fast exit; this is the safety net for duels that never even start
/// fighting. See `AnimalFight::idle_ticks`.
pub const IDLE_DUEL_TICKS: u32 = 300;

/// Intensity raised by one landed blow (either direction, POLICY: the
/// doc names the meter but no legacy number survives).
pub const INTENSITY_PER_BLOW: i32 = 2500;
/// Intensity de-escalation per combat tick (10 Hz) without a blow
/// (POLICY: a hot battle cools in ~7 s of no blows).
pub const INTENSITY_DECAY: i32 = 250;

/// Aggro-leash give-up distance (session 83; POLICY: the wiki keeps no
/// legacy number - a chase feels "won" by escape after a good sprint).
/// An aggressor this far (Manhattan, subtiles) from its home anchor
/// stops chasing, tears down its duel and walks home. ~55 tiles.
pub const AGGRO_GIVEUP: i32 = 600;
/// A surrendered aggressor is "home" again inside this radius of its
/// anchor and may re-aggro (hysteresis that keeps the march back calm).
pub const AGGRO_ARRIVED: i32 = 60;
