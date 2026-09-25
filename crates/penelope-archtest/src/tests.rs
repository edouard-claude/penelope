use super::*;

#[test]
fn the_workspace_is_discovered() {
    let all = crates();
    assert!(all.len() >= 15, "crates trouvés : {}", all.len());
    let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
    for expected in [
        "penelope-kernel",
        "penelope-store",
        "penelope-platform",
        "penelope-mcp",
        "penelope-memory",
        "penelope-daemon",
        "penelope-app",
        "penelope-mcp-host",
        "penelope-vault",
        "penelope-ops",
        "penelope-dream",
        "penelope-agent",
        "penelope-executor",
        "penelope-conversation",
        "penelope-orchestrator",
        "penelope-cli",
        "penelope-gateway-telegram",
    ] {
        assert!(names.contains(&expected), "crate manquant : {expected}");
    }
}

/// CA 3 : le test d'architecture échoue si une dépendance interdite est ajoutée.
#[test]
fn ca_3_1_dependency_rules_hold() {
    let v = dependency_violations();
    assert!(v.is_empty(), "violations :\n{}", v.join("\n"));
}

#[test]
fn store_depends_on_no_business_crate() {
    let all = crates();
    let store = all.iter().find(|c| c.name == "penelope-store").unwrap();
    assert!(
        store.internal_deps.is_empty(),
        "penelope-store doit rester une infrastructure pure : {:?}",
        store.internal_deps
    );
}

#[test]
fn kernel_depends_only_on_store() {
    let all = crates();
    let kernel = all.iter().find(|c| c.name == "penelope-kernel").unwrap();
    assert_eq!(
        kernel.internal_deps,
        ["penelope-store".to_string()].into_iter().collect(),
        "le noyau ne dépend que du stockage"
    );
}

/// T36 : le daemon ne connaît plus la bibliothèque du canal ; seule la passerelle,
/// au-dessus de lui, s'en sert.
#[test]
fn the_daemon_does_not_depend_on_the_channel_library() {
    let all = crates();
    let daemon = all.iter().find(|c| c.name == "penelope-daemon").unwrap();
    assert!(
        !daemon.internal_deps.contains("penelope-telegram"),
        "penelope-daemon dépend de penelope-telegram : {:?}",
        daemon.internal_deps
    );
}

/// T21 : le socle de l'application ne connaît pas le daemon ; T36 : ni le canal,
/// qu'il n'atteint que par ses ports (`Cards`, `ChannelDelivery`, `OwnerChannel`).
#[test]
fn the_app_crate_sees_neither_the_daemon_nor_the_channel() {
    let all = crates();
    let app = all.iter().find(|c| c.name == "penelope-app").unwrap();
    for above in [
        "penelope-daemon",
        "penelope-telegram",
        "penelope-gateway-telegram",
    ] {
        assert!(
            !app.internal_deps.contains(above),
            "penelope-app dépend de {above} : {:?}",
            app.internal_deps
        );
    }
}

/// T25 : l'hôte MCP ne connaît pas le daemon, qui le construit.
#[test]
fn the_mcp_host_crate_does_not_depend_on_the_daemon() {
    let all = crates();
    let host = all.iter().find(|c| c.name == "penelope-mcp-host").unwrap();
    assert!(
        !host.internal_deps.contains("penelope-daemon"),
        "penelope-mcp-host est sous le daemon : {:?}",
        host.internal_deps
    );
}

/// T29 : la passerelle Telegram n'est composée que par la CLI.
#[test]
fn only_the_cli_depends_on_the_gateway() {
    let v = gateway_dependent_violations();
    assert!(v.is_empty(), "violations :\n{}", v.join("\n"));
    let all = crates();
    let cli = all.iter().find(|c| c.name == "penelope-cli").unwrap();
    assert!(
        cli.internal_deps.contains("penelope-gateway-telegram"),
        "la CLI compose la passerelle : {:?}",
        cli.internal_deps
    );
}

/// T22 : la mémoire en fichiers ne connaît pas le daemon.
#[test]
fn the_vault_crate_does_not_depend_on_the_daemon() {
    let all = crates();
    let vault = all.iter().find(|c| c.name == "penelope-vault").unwrap();
    assert!(
        !vault.internal_deps.contains("penelope-daemon"),
        "penelope-vault est sous le daemon : {:?}",
        vault.internal_deps
    );
}

/// T28 : l'exploitation ne connaît ni le daemon, qui compose `doctor`, ni l'hôte MCP.
#[test]
fn the_ops_crate_depends_neither_on_the_daemon_nor_on_the_mcp_host() {
    let all = crates();
    let ops = all.iter().find(|c| c.name == "penelope-ops").unwrap();
    for above in ["penelope-daemon", "penelope-mcp-host"] {
        assert!(
            !ops.internal_deps.contains(above),
            "penelope-ops est sous {above} : {:?}",
            ops.internal_deps
        );
    }
}

/// T26 : le rêve, l'ingestion et l'accueil ne connaissent pas le daemon.
#[test]
fn the_dream_crate_does_not_depend_on_the_daemon() {
    let all = crates();
    let dream = all.iter().find(|c| c.name == "penelope-dream").unwrap();
    assert!(
        !dream.internal_deps.contains("penelope-daemon"),
        "penelope-dream est sous le daemon : {:?}",
        dream.internal_deps
    );
}

/// T24 : l'exécuteur ne connaît ni le daemon, ni la boucle qui l'appelle, ni les
/// crates qui sont au-dessus de lui ; il ne les atteint que par les ports.
#[test]
fn the_executor_crate_sees_neither_the_daemon_nor_the_agent_loop() {
    let all = crates();
    let executor = all.iter().find(|c| c.name == "penelope-executor").unwrap();
    for above in [
        "penelope-daemon",
        "penelope-agent",
        "penelope-orchestrator",
        "penelope-dream",
        "penelope-ops",
        "penelope-mcp-host",
        "penelope-gateway-telegram",
        // T36 : les commandes du canal viennent du canal branché.
        "penelope-telegram",
    ] {
        assert!(
            !executor.internal_deps.contains(above),
            "penelope-executor dépend de {above} : {:?}",
            executor.internal_deps
        );
    }
}

/// T10 : la boucle ne voit ni le moteur de contexte, ni la mémoire, ni le canal, ni
/// le daemon (`design/v1/README.md` §3.2).
#[test]
fn the_agent_crate_sees_neither_context_memory_channel_nor_daemon() {
    let all = crates();
    let agent = all.iter().find(|c| c.name == "penelope-agent").unwrap();
    for above in [
        "penelope-context",
        "penelope-memory",
        "penelope-telegram",
        "penelope-daemon",
    ] {
        assert!(
            !agent.internal_deps.contains(above),
            "penelope-agent dépend de {above} : {:?}",
            agent.internal_deps
        );
    }
}

/// T23 : la conversation ne voit ni le daemon, ni la boucle qui la consomme, ni le
/// canal (`design/v1/README.md` §3.2).
#[test]
fn the_conversation_crate_sees_neither_daemon_loop_nor_channel() {
    let all = crates();
    let conversation = all
        .iter()
        .find(|c| c.name == "penelope-conversation")
        .unwrap();
    for above in ["penelope-daemon", "penelope-agent", "penelope-telegram"] {
        assert!(
            !conversation.internal_deps.contains(above),
            "penelope-conversation dépend de {above} : {:?}",
            conversation.internal_deps
        );
    }
}

/// T27 : l'orchestrateur ne voit ni le daemon, qui le compose, ni le canal, ni les
/// crates d'exploitation et d'hôte MCP, qu'il n'atteint que par les ports.
#[test]
fn the_orchestrator_crate_sees_neither_the_daemon_nor_the_channel() {
    let all = crates();
    let orchestrator = all
        .iter()
        .find(|c| c.name == "penelope-orchestrator")
        .unwrap();
    for above in [
        "penelope-daemon",
        "penelope-gateway-telegram",
        "penelope-telegram",
        "penelope-ops",
        "penelope-mcp-host",
    ] {
        assert!(
            !orchestrator.internal_deps.contains(above),
            "penelope-orchestrator dépend de {above} : {:?}",
            orchestrator.internal_deps
        );
    }
    for below in ["penelope-agent", "penelope-executor", "penelope-dream"] {
        let crate_below = all.iter().find(|c| c.name == below).unwrap();
        assert!(
            !crate_below.internal_deps.contains("penelope-orchestrator"),
            "{below} est sous l'orchestrateur : {:?}",
            crate_below.internal_deps
        );
    }
}

#[test]
fn there_is_no_dependency_cycle() {
    let c = dependency_cycles();
    assert!(c.is_empty(), "cycles :\n{}", c.join("\n"));
}

/// CA 2 : le test échoue si un chemin littéral, un appel shell ou une API propre à un
/// OS apparaît hors de `penelope-platform`.
#[test]
fn ca_2_3_no_os_specific_code_outside_the_platform_crate() {
    let v = forbidden_patterns();
    assert!(
        v.is_empty(),
        "motifs interdits :\n{}",
        v.iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn no_os_specific_dependencies_outside_the_platform_crate() {
    let v = os_dependency_violations();
    assert!(v.is_empty(), "{}", v.join("\n"));
}

#[test]
fn unsafe_is_forbidden_everywhere() {
    let v = unsafe_violations();
    assert!(v.is_empty(), "{}", v.join("\n"));
}

#[test]
fn the_pattern_detector_actually_detects() {
    // Garde-fou : si la détection cassait, les tests ci-dessus passeraient à tort.
    let sample = "let p = \"~/Library/Application Support/x\";";
    assert!(
        FORBIDDEN_PATTERNS
            .iter()
            .any(|(_, needle)| sample.contains(needle)),
        "le détecteur de motifs ne détecte plus rien"
    );
    let shell = "Command::new(\"bash\").arg(\"-c\")";
    assert!(
        FORBIDDEN_PATTERNS
            .iter()
            .any(|(_, needle)| shell.contains(needle))
    );
}

#[test]
fn test_files_are_exempt_from_the_forbidden_patterns() {
    let raw = "let p = \"/tmp/projet\";\n";
    for rel in [
        "crates/penelope-daemon/src/workflow/tests.rs",
        "crates/penelope-daemon/src/telegram/tests/commands.rs",
        "crates/penelope-daemon/src/agent/clone_policy_tests.rs",
        "crates/penelope-daemon/src/mcp/testing.rs",
    ] {
        let v = forbidden_patterns_in("penelope-daemon", Path::new(rel), raw);
        assert!(v.is_empty(), "{rel} est un fichier de tests : {v:?}");
    }
    let v = forbidden_patterns_in(
        "penelope-daemon",
        Path::new("crates/penelope-daemon/src/workflow.rs"),
        raw,
    );
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].rule, "chemin absolu /tmp");
    assert_eq!(v[0].line, 1);
}

#[test]
fn every_crate_has_sources() {
    for c in crates() {
        assert!(
            !sources(&c).is_empty(),
            "{} n'a aucun fichier source",
            c.name
        );
    }
}
