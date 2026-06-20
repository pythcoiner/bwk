//! The variable-time ECDH multiply used for recipient scanning must produce the exact same point as
//! the constant-time multiply. Only the timing differs.
use silentpayments::secp256k1::{
    ecdh::{shared_secret_point, shared_secret_point_vartime},
    rand, PublicKey, Secp256k1, SecretKey,
};

#[test]
fn vartime_matches_const_time() {
    let secp = Secp256k1::new();
    let mut rng = rand::thread_rng();

    for _ in 0..1000 {
        let scalar = SecretKey::new(&mut rng);
        let point = PublicKey::from_secret_key(&secp, &SecretKey::new(&mut rng));

        let const_time = shared_secret_point(&point, &scalar);
        let vartime = shared_secret_point_vartime(&point, &scalar);

        assert_eq!(const_time, vartime);
    }
}
