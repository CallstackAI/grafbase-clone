use std::borrow::Cow;

use sha2::{Digest, Sha256};

use super::*;

const EXPECTED_SHA: &str = "349bfc36ec2fbd96509ac760f07bc00747f5d431b5b516204e45ec9fb9eb9519";

#[test]
fn test_serde_roundtrip() {
    let id = r"
            This test ensures the default `VersionedRegistry` serialization output remains stable.

            When this test fails, it likely means the shape of the `Registry` type was updated,
            which can cause backward-incompatibility issues.

            Before updating this test to match the expected result, please ensure the changes to
            `Registry` are applied in a backward compatible way.

            One way to do so, is to have the `Default` trait return a value that keeps the existing
            expectation, and `#[serde(default)]` is applied to any newly added field.

            Once you are satisfied your changes are backward-compatible, update `EXPECTED_SHA` with
            the new output presented in the test result.
        ";

    let registry = registry_upgrade::convert_v1_to_v2(Registry::new().with_sample_data()).unwrap();
    let versioned_registry = VersionedRegistry {
        registry,
        deployment_id: Cow::Borrowed(id),
    };
    let serialized_versioned_registry = serde_json::to_string(&versioned_registry).unwrap();
    let serialized_sha = Sha256::digest(serialized_versioned_registry);

    assert_eq!(&format!("{serialized_sha:x}"), EXPECTED_SHA);
}
