use super::*;

/// #135 : un modèle qui coupe au-delà de 10 candidats juge 188 groupes en au plus
/// 25 appels ; après une descente à 5, le lot suivant part entre 5 et 10.
#[test]
fn batch_sizes_remember_what_held() {
    let mut sizer = BatchSizer::new(40);
    let (mut done, mut calls) = (0usize, 0usize);
    while done < 188 {
        let size = sizer.size(188 - done);
        calls += 1;
        if size > 10 {
            sizer.cut(size);
        } else {
            sizer.ok(size);
            done += size;
        }
    }
    assert!(calls <= 25, "{calls} appels");

    let mut sizer = BatchSizer::new(40);
    sizer.cut(40);
    sizer.cut(20);
    sizer.cut(10);
    assert_eq!(sizer.size(100), 5);
    sizer.ok(5);
    let next = sizer.size(100);
    assert!((5..=10).contains(&next), "{next}");
    let budget = OutputBudget::new(16_000);
    assert!(
        budget.max_tokens(40) > 40 * 120,
        "plus que 120 tokens par candidat"
    );
    assert!(budget.fit(40) * 455 <= 16_000 + 455);
}

/// #140 : après des coupures à 40, 20, 10, 5 et 2 sur la même tête de lot, qui ne
/// tient qu'à un candidat, le lot suivant ne fait pas 1 : seule la première coupure
/// accuse la taille. Deux fois de suite, c'est la taille. Une coupure à 2 ne pose
/// jamais un plafond à 1.
#[test]
fn a_heavy_head_does_not_shrink_every_later_batch() {
    let mut sizer = BatchSizer::new(40);
    sizer.ok(40);
    for size in [40, 20, 10, 5, 2] {
        assert_eq!(sizer.size(200), size);
        sizer.cut(size);
    }
    assert_eq!(sizer.size(200), 1);
    sizer.ok(1);
    assert_eq!(sizer.size(200), 20);
    sizer.ok(20);
    assert_eq!(sizer.size(200), 30);

    // Deux rejeux de suite jusqu'à 1 : la taille est en cause, la plus petite coupure
    // au-dessus de 2 (3) devient le plafond, et le lot suivant fait 2, pas 1.
    for size in [30, 15, 7, 3] {
        sizer.cut(size);
    }
    sizer.ok(1);
    for size in [20, 10, 5, 3] {
        sizer.cut(size);
    }
    sizer.ok(1);
    assert_eq!(sizer.size(200), 2);
    sizer.cut(2);
    sizer.ok(1);
    assert_eq!(
        sizer.size(200),
        2,
        "une coupure à 2 ne pose pas de plafond à 1"
    );
}

/// #140 : les lots faciles de l'essai à blanc tiraient l'estimation à ~191 tokens par
/// candidat ; la coupure du lot de 40 à 9 958 tokens la remonte au-dessus de la
/// preuve, et les lots faciles qui suivent ne la font plus redescendre dessous. Le
/// plancher double quand c'est lui qui a coupé.
#[test]
fn a_cut_teaches_the_output_estimate() {
    let mut budget = OutputBudget::new(32_000);
    budget.observe(40, 17_927);
    budget.observe(40, 3_069);
    budget.observe(40, 5_339);
    let before = budget.max_tokens(40);
    assert!((9_800..10_000).contains(&before), "{before}");
    budget.cut(40, before, 9_958);
    assert!(budget.max_tokens(20) as f64 >= 20.0 * 9_958.0 / 40.0 * 1.3 - 1.0);
    budget.observe(40, 3_069);
    assert!(budget.max_tokens(40) as f64 >= 9_958.0 * 1.3 - 1.0);

    let mut budget = OutputBudget::new(16_000);
    assert_eq!(budget.max_tokens(1), 2_000);
    budget.cut(2, 2_000, 2_000);
    assert_eq!(budget.max_tokens(1), 4_000, "plancher doublé");
    assert!(
        budget.fit(40) >= 30,
        "l'estimation par candidat ne bouge pas"
    );
    budget.cut(1, 4_000, 4_000);
    assert_eq!(budget.max_tokens(1), 8_000);
    budget.cut(1, 8_000, 8_000);
    budget.cut(1, 16_000, 16_000);
    assert_eq!(
        budget.max_tokens(1),
        16_000,
        "jamais au-delà de la limite du modèle"
    );
}

/// #152 : 240 s fixes tuaient tout appel qui réfléchissait vraiment. Le 21/09, avec
/// 8 000 à 16 000 tokens de raisonnement autorisés, chaque lot demandait cinq à onze
/// minutes : tué à quatre, classé « réseau coupé », rejoué après 300 s, sans fin.
#[test]
fn the_call_deadline_follows_the_budget_it_was_given() {
    let hour = Duration::from_secs(3600);
    // Un petit budget garde le plancher : rien ne justifie d'attendre plus.
    assert_eq!(call_timeout(1_000, hour), CALL_TIMEOUT_MIN);
    // Le budget qui a fait échouer la nuit : 16 000 de raisonnement + la sortie.
    assert!(
        call_timeout(24_000, hour) > Duration::from_secs(240),
        "un lot qui réfléchit a plus de quatre minutes"
    );
    // Mais jamais sans borne : un seul appel n'immobilise pas la nuit.
    assert_eq!(call_timeout(1_000_000, hour), CALL_TIMEOUT_MAX);
    // Ni au-delà de ce qu'il reste à la passe.
    assert_eq!(
        call_timeout(1_000_000, Duration::from_secs(300)),
        Duration::from_secs(300),
        "le reste de la nuit borne le délai"
    );
    // Mais le plancher tient : un reste dérisoire ne donne pas un appel mort-né.
    assert_eq!(
        call_timeout(1_000_000, Duration::from_secs(10)),
        CALL_TIMEOUT_MIN
    );
}

/// #152 : « notre appel a dépassé son délai » n'est pas « la machine dormait ». Les
/// confondre coûtait 300 s d'attente par tentative, indéfiniment.
#[test]
fn our_own_deadline_is_not_a_network_cut() {
    let mine: anyhow::Error = LlmError::new(
        LlmErrorKind::Transient,
        format!("{OWN_TIMEOUT} : rien de complet en 600 s"),
    )
    .into();
    assert!(own_timeout(&mine));
    assert!(
        !network_stall(&mine),
        "sans sonde réseau, notre délai ne prouve aucune coupure"
    );

    // Une vraie coupure locale, elle, reste reconnue.
    let cut: anyhow::Error = LlmError::new(
        LlmErrorKind::Transient,
        "error sending request for url".to_string(),
    )
    .into();
    assert!(network_stall(&cut));
    assert!(!own_timeout(&cut));

    // Et l'erreur du fournisseur de #127 n'est ni l'un ni l'autre.
    let upstream: anyhow::Error = LlmError::new(
        LlmErrorKind::Transient,
        "Upstream idle timeout exceeded".to_string(),
    )
    .into();
    assert!(!network_stall(&upstream));
    assert!(!own_timeout(&upstream));
}
