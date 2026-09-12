// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! `ClusterConfig::from_env` (`test_config.py`), exercised through an
//! in-memory environment.

use a2x_cluster::ClusterConfig;
use ap_support::env::MapEnv;
use ap_support::testing::TestResult;

#[test]
fn defaults_when_no_env() -> TestResult {
    let cfg = ClusterConfig::from_env_source(&MapEnv::new());
    assert_eq!((cfg.hold_timeout, cfg.keepalive_interval), (30.0, 10.0));
    assert_eq!(cfg.tombstone_retention(), 40.0);
    Ok(())
}

#[test]
fn override_hold_and_keepalive() -> TestResult {
    let env = MapEnv::from([
        ("A2X_REGISTRY_CLUSTER_HOLD_TIMEOUT", "60"),
        ("A2X_REGISTRY_CLUSTER_KEEPALIVE_INTERVAL", "20"),
        ("A2X_REGISTRY_CLUSTER_ANTI_ENTROPY_INTERVAL", "7.5"),
    ]);
    let cfg = ClusterConfig::from_env_source(&env);
    assert_eq!(cfg.hold_timeout, 60.0);
    assert_eq!(cfg.keepalive_interval, 20.0);
    assert_eq!(cfg.tombstone_retention(), 80.0);
    assert_eq!(cfg.anti_entropy_interval, 7.5);
    assert_eq!(cfg.http_timeout, 5.0);
    Ok(())
}

#[test]
fn override_scale_knobs_and_int_from_float_string() -> TestResult {
    let env = MapEnv::from([
        ("A2X_REGISTRY_CLUSTER_BROADCAST_WORKERS", "64"),
        ("A2X_REGISTRY_CLUSTER_MERKLE_BUCKETS", "512.0"),
        ("A2X_REGISTRY_CLUSTER_HOLD_TIMEOUT", "20"),
    ]);
    let cfg = ClusterConfig::from_env_source(&env);
    assert_eq!(cfg.broadcast_workers, 64);
    assert_eq!(cfg.merkle_buckets, 512);
    assert_eq!(cfg.hold_timeout, 20.0);
    Ok(())
}

#[test]
fn invalid_or_blank_value_falls_back_to_default() -> TestResult {
    let env = MapEnv::from([
        ("A2X_REGISTRY_CLUSTER_HOLD_TIMEOUT", "abc"),
        ("A2X_REGISTRY_CLUSTER_MERKLE_BUCKETS", "  "),
    ]);
    let cfg = ClusterConfig::from_env_source(&env);
    assert_eq!(cfg.hold_timeout, 30.0);
    assert_eq!(cfg.merkle_buckets, 256);
    Ok(())
}
