//! 加密扩展：对齐 Legado JsEncodeUtils 的 java.* 加密方法。
//!
//! 实现摘要（md5 / sha1 / sha256 / sha512）、HMac、AES（ECB/CBC，
//! PKCS7/NoPadding），供 js.rs 绑定到 JS 的 `java` 对象。
//! 关键语义（均从官方源码确认）：
//! - `md5Encode` 32 位小写 hex；`md5Encode16` 取 [8..24]
//! - `digestHex(data, algorithm)` / `digestBase64Str(data, algorithm)`：
//!   algorithm 兼容 "MD5" / "SHA-1" / "SHA1" / "SHA-256" / "SHA256" 等写法
//! - `HMacHex(data, algorithm, key)` / `HMacBase64(...)`：参数顺序 data, algorithm, key
//! - AES：transformation 形如 "AES/CBC/PKCS5Padding"，key/iv 为原始字符串字节，
//!   Base64 变体的入参（key/iv）先做 Base64 解码；PKCS5 视为 PKCS7。

use crate::error::{AppError, AppResult};
use aes::cipher::block_padding::{NoPadding, Pkcs7, ZeroPadding};
use aes::cipher::{BlockDecryptMut, BlockEncryptMut};
use base64::Engine;
use des::{Des, TdesEde2, TdesEde3};
use digest::Digest;
use hmac::{Hmac, Mac};
use md5::Md5;
use sha1::Sha1;
use sha2::{Sha224, Sha256, Sha384, Sha512};

type Aes128EcbEnc = ecb::Encryptor<aes::Aes128>;
type Aes128EcbDec = ecb::Decryptor<aes::Aes128>;
type Aes192EcbEnc = ecb::Encryptor<aes::Aes192>;
type Aes192EcbDec = ecb::Decryptor<aes::Aes192>;
type Aes256EcbEnc = ecb::Encryptor<aes::Aes256>;
type Aes256EcbDec = ecb::Decryptor<aes::Aes256>;
type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
type Aes192CbcEnc = cbc::Encryptor<aes::Aes192>;
type Aes192CbcDec = cbc::Decryptor<aes::Aes192>;
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

type DesEcbEnc = ecb::Encryptor<Des>;
type DesEcbDec = ecb::Decryptor<Des>;
type DesCbcEnc = cbc::Encryptor<Des>;
type DesCbcDec = cbc::Decryptor<Des>;
type Tdes2EcbEnc = ecb::Encryptor<TdesEde2>;
type Tdes2EcbDec = ecb::Decryptor<TdesEde2>;
type Tdes2CbcEnc = cbc::Encryptor<TdesEde2>;
type Tdes2CbcDec = cbc::Decryptor<TdesEde2>;
type Tdes3EcbEnc = ecb::Encryptor<TdesEde3>;
type Tdes3EcbDec = ecb::Decryptor<TdesEde3>;
type Tdes3CbcEnc = cbc::Encryptor<TdesEde3>;
type Tdes3CbcDec = cbc::Decryptor<TdesEde3>;

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

// ---------------- 摘要 ----------------

/// 规范化算法名：去掉横杠与大小写，统一为小写字母数字（SHA-256 -> sha256）
fn norm_alg(algorithm: &str) -> String {
    algorithm.chars().filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase()).collect()
}

/// 通用摘要 → hex。支持 MD5/SHA1/SHA224/SHA256/SHA384/SHA512
pub fn digest_hex(data: &str, algorithm: &str) -> AppResult<String> {
    let bytes = data.as_bytes();
    let hex = match norm_alg(algorithm).as_str() {
        "md5" | "md5digest" => hex::encode(Md5::digest(bytes)),
        "sha1" => hex::encode(Sha1::digest(bytes)),
        "sha224" => hex::encode(Sha224::digest(bytes)),
        "sha256" => hex::encode(Sha256::digest(bytes)),
        "sha384" => hex::encode(Sha384::digest(bytes)),
        "sha512" => hex::encode(Sha512::digest(bytes)),
        other => return Err(AppError::bad_request(format!("不支持的摘要算法: {}", other))),
    };
    Ok(hex)
}

/// 通用摘要 → Base64（无换行，对齐官方 NO_WRAP）
pub fn digest_base64(data: &str, algorithm: &str) -> AppResult<String> {
    let bytes = data.as_bytes();
    let out = match norm_alg(algorithm).as_str() {
        "md5" | "md5digest" => b64().encode(Md5::digest(bytes)),
        "sha1" => b64().encode(Sha1::digest(bytes)),
        "sha224" => b64().encode(Sha224::digest(bytes)),
        "sha256" => b64().encode(Sha256::digest(bytes)),
        "sha384" => b64().encode(Sha384::digest(bytes)),
        "sha512" => b64().encode(Sha512::digest(bytes)),
        other => return Err(AppError::bad_request(format!("不支持的摘要算法: {}", other))),
    };
    Ok(out)
}

pub fn md5_encode(data: &str) -> String { hex::encode(Md5::digest(data.as_bytes())) }

/// 官方 md5Encode16：取 32 位 md5 的 [8..24]
pub fn md5_encode16(data: &str) -> String {
    let full = md5_encode(data);
    full.get(8..24).unwrap_or(&full).to_string()
}

// ---------------- HMac ----------------

macro_rules! hmac_digest {
    ($mac:ty, $data:expr, $key:expr, $hex:expr) => {{
        let mut m = <$mac>::new_from_slice($key)
            .map_err(|e| AppError::bad_request(format!("HMac 密钥错误: {}", e)))?;
        m.update($data);
        let out = m.finalize().into_bytes();
        if $hex { hex::encode(out) } else { b64().encode(out) }
    }};
}

/// HMac(data, algorithm, key) → hex。参数顺序对齐官方 HMacHex
pub fn hmac_hex(data: &str, algorithm: &str, key: &str) -> AppResult<String> {
    hmac_impl(data, algorithm, key.as_bytes(), true)
}

/// HMac(data, algorithm, key) → Base64
pub fn hmac_base64(data: &str, algorithm: &str, key: &str) -> AppResult<String> {
    hmac_impl(data, algorithm, key.as_bytes(), false)
}

fn hmac_impl(data: &str, algorithm: &str, key: &[u8], as_hex: bool) -> AppResult<String> {
    let d = data.as_bytes();
    Ok(match norm_alg(algorithm).as_str() {
        "md5" => hmac_digest!(Hmac<Md5>, d, key, as_hex),
        "sha1" => hmac_digest!(Hmac<Sha1>, d, key, as_hex),
        "sha224" => hmac_digest!(Hmac<Sha224>, d, key, as_hex),
        "sha256" => hmac_digest!(Hmac<Sha256>, d, key, as_hex),
        "sha384" => hmac_digest!(Hmac<Sha384>, d, key, as_hex),
        "sha512" => hmac_digest!(Hmac<Sha512>, d, key, as_hex),
        other => return Err(AppError::bad_request(format!("不支持的 HMac 算法: {}", other))),
    })
}

// ---------------- AES ----------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum AesMode { Ecb, Cbc }

#[derive(Clone, Copy, PartialEq)]
enum AesPadding { Pkcs7, Zero, None }

/// 对称密码族：transformation 首段决定（AES / DES / DESede(3DES)）
#[derive(Clone, Copy, PartialEq, Debug)]
enum SymAlg { Aes, Des, Tdes }

/// 解析 transformation："AES/CBC/PKCS5Padding" → (算法, CBC, PKCS7)
/// 兼容官方 JCA 写法：PKCS5Padding 在 AES(16字节块) 下等价于 PKCS7Padding
fn parse_transformation(t: &str) -> (SymAlg, AesMode, AesPadding) {
    let parts: Vec<&str> = t.split('/').collect();
    let head = parts.first().map(|s| norm_alg(s)).unwrap_or_default();
    let alg = match head.as_str() {
        "des" => SymAlg::Des,
        "desede" | "3des" | "tripledes" | "tdes" => SymAlg::Tdes,
        _ => SymAlg::Aes, // 默认 AES（与 JCA "AES" 一致）
    };
    let mode = if parts.get(1).map(|s| s.eq_ignore_ascii_case("cbc")).unwrap_or(false) {
        AesMode::Cbc
    } else {
        AesMode::Ecb // 默认 ECB（与 JCA "AES" 默认一致）
    };
    let padding = match parts.get(2).map(|s| s.to_ascii_lowercase()) {
        Some(p) if p.contains("pkcs") => AesPadding::Pkcs7, // PKCS5/PKCS7 均映射
        Some(p) if p.contains("zero") || p.contains("iso") => AesPadding::Zero,
        Some(p) if p.contains("nopadding") || p == "raw" => AesPadding::None,
        _ => AesPadding::Pkcs7,
    };
    (alg, mode, padding)
}

/// 通用对称加密 → 原始字节。
/// key/iv 传入的是原始字符串字节（官方 createSymmetricCrypto 对 String 直接 encodeToByteArray）。
/// DES：key 8 字节；3DES：key 16 字节（K1K2K1，对齐 Java DESede）/ 24 字节。
///
/// ECB 的 cipher 只实现 KeyInit（`new_from_slice(key)` 单参数）；
/// CBC 的 cipher 实现 KeyIvInit（`new_from_slices(key, iv)` 双参数）。
/// 因此拆成两组独立宏，避免运行时 if 让两支都参与类型检查。
macro_rules! enc_ecb {
    ($cipher:ty, $pad:ty, $data:expr, $key:expr) => {{
        Ok(<$cipher as aes::cipher::KeyInit>::new_from_slice($key)
            .map_err(|_| AppError::bad_request("密钥长度非法".to_string()))?
            .encrypt_padded_vec_mut::<$pad>($data))
    }};
}
macro_rules! enc_cbc {
    ($cipher:ty, $pad:ty, $data:expr, $key:expr, $iv:expr) => {{
        Ok(<$cipher as aes::cipher::KeyIvInit>::new_from_slices($key, $iv)
            .map_err(|_| AppError::bad_request("密钥/IV 长度非法".to_string()))?
            .encrypt_padded_vec_mut::<$pad>($data))
    }};
}
macro_rules! dispatch_enc_ecb {
    ($cipher:ty, $padding:expr, $data:expr, $key:expr) => {
        match $padding {
            AesPadding::Pkcs7 => enc_ecb!($cipher, Pkcs7, $data, $key),
            AesPadding::Zero  => enc_ecb!($cipher, ZeroPadding, $data, $key),
            AesPadding::None  => enc_ecb!($cipher, NoPadding, $data, $key),
        }
    };
}
macro_rules! dispatch_enc_cbc {
    ($cipher:ty, $padding:expr, $data:expr, $key:expr, $iv:expr) => {
        match $padding {
            AesPadding::Pkcs7 => enc_cbc!($cipher, Pkcs7, $data, $key, $iv),
            AesPadding::Zero  => enc_cbc!($cipher, ZeroPadding, $data, $key, $iv),
            AesPadding::None  => enc_cbc!($cipher, NoPadding, $data, $key, $iv),
        }
    };
}

fn sym_encrypt_bytes(alg: SymAlg, data: &[u8], key: &[u8], iv: &[u8], t: &str) -> AppResult<Vec<u8>> {
    let (parsed_alg, mode, padding) = parse_transformation(t);
    let alg = if parsed_alg != alg { parsed_alg } else { alg };
    match (alg, key.len(), mode) {
        (SymAlg::Aes, 16, AesMode::Ecb) => dispatch_enc_ecb!(Aes128EcbEnc, padding, data, key),
        (SymAlg::Aes, 16, AesMode::Cbc) => dispatch_enc_cbc!(Aes128CbcEnc, padding, data, key, iv),
        (SymAlg::Aes, 24, AesMode::Ecb) => dispatch_enc_ecb!(Aes192EcbEnc, padding, data, key),
        (SymAlg::Aes, 24, AesMode::Cbc) => dispatch_enc_cbc!(Aes192CbcEnc, padding, data, key, iv),
        (SymAlg::Aes, 32, AesMode::Ecb) => dispatch_enc_ecb!(Aes256EcbEnc, padding, data, key),
        (SymAlg::Aes, 32, AesMode::Cbc) => dispatch_enc_cbc!(Aes256CbcEnc, padding, data, key, iv),
        (SymAlg::Des, 8, AesMode::Ecb) => dispatch_enc_ecb!(DesEcbEnc, padding, data, key),
        (SymAlg::Des, 8, AesMode::Cbc) => dispatch_enc_cbc!(DesCbcEnc, padding, data, key, iv),
        (SymAlg::Tdes, 16, AesMode::Ecb) => dispatch_enc_ecb!(Tdes2EcbEnc, padding, data, key),
        (SymAlg::Tdes, 16, AesMode::Cbc) => dispatch_enc_cbc!(Tdes2CbcEnc, padding, data, key, iv),
        (SymAlg::Tdes, 24, AesMode::Ecb) => dispatch_enc_ecb!(Tdes3EcbEnc, padding, data, key),
        (SymAlg::Tdes, 24, AesMode::Cbc) => dispatch_enc_cbc!(Tdes3CbcEnc, padding, data, key, iv),
        (SymAlg::Aes, n, _) => Err(AppError::bad_request(format!("AES 密钥长度须为 16/24/32 字节，实际 {}", n))),
        (SymAlg::Des, n, _) => Err(AppError::bad_request(format!("DES 密钥长度须为 8 字节，实际 {}", n))),
        (SymAlg::Tdes, n, _) => Err(AppError::bad_request(format!("3DES 密钥长度须为 16/24 字节，实际 {}", n))),
    }
}

macro_rules! dec_ecb {
    ($cipher:ty, $pad:ty, $data:expr, $key:expr) => {{
        Ok(<$cipher as aes::cipher::KeyInit>::new_from_slice($key)
            .map_err(|_| AppError::bad_request("密钥长度非法".to_string()))?
            .decrypt_padded_vec_mut::<$pad>($data)
            .map_err(|_| AppError::bad_request("解密失败：填充或密钥错误".to_string()))?)
    }};
}
macro_rules! dec_cbc {
    ($cipher:ty, $pad:ty, $data:expr, $key:expr, $iv:expr) => {{
        Ok(<$cipher as aes::cipher::KeyIvInit>::new_from_slices($key, $iv)
            .map_err(|_| AppError::bad_request("密钥/IV 长度非法".to_string()))?
            .decrypt_padded_vec_mut::<$pad>($data)
            .map_err(|_| AppError::bad_request("解密失败：填充或密钥错误".to_string()))?)
    }};
}
macro_rules! dispatch_dec_ecb {
    ($cipher:ty, $padding:expr, $data:expr, $key:expr) => {
        match $padding {
            AesPadding::Pkcs7 => dec_ecb!($cipher, Pkcs7, $data, $key),
            AesPadding::Zero  => dec_ecb!($cipher, ZeroPadding, $data, $key),
            AesPadding::None  => dec_ecb!($cipher, NoPadding, $data, $key),
        }
    };
}
macro_rules! dispatch_dec_cbc {
    ($cipher:ty, $padding:expr, $data:expr, $key:expr, $iv:expr) => {
        match $padding {
            AesPadding::Pkcs7 => dec_cbc!($cipher, Pkcs7, $data, $key, $iv),
            AesPadding::Zero  => dec_cbc!($cipher, ZeroPadding, $data, $key, $iv),
            AesPadding::None  => dec_cbc!($cipher, NoPadding, $data, $key, $iv),
        }
    };
}

fn sym_decrypt_bytes(alg: SymAlg, data: &[u8], key: &[u8], iv: &[u8], t: &str) -> AppResult<Vec<u8>> {
    let (parsed_alg, mode, padding) = parse_transformation(t);
    let alg = if parsed_alg != alg { parsed_alg } else { alg };
    match (alg, key.len(), mode) {
        (SymAlg::Aes, 16, AesMode::Ecb) => dispatch_dec_ecb!(Aes128EcbDec, padding, data, key),
        (SymAlg::Aes, 16, AesMode::Cbc) => dispatch_dec_cbc!(Aes128CbcDec, padding, data, key, iv),
        (SymAlg::Aes, 24, AesMode::Ecb) => dispatch_dec_ecb!(Aes192EcbDec, padding, data, key),
        (SymAlg::Aes, 24, AesMode::Cbc) => dispatch_dec_cbc!(Aes192CbcDec, padding, data, key, iv),
        (SymAlg::Aes, 32, AesMode::Ecb) => dispatch_dec_ecb!(Aes256EcbDec, padding, data, key),
        (SymAlg::Aes, 32, AesMode::Cbc) => dispatch_dec_cbc!(Aes256CbcDec, padding, data, key, iv),
        (SymAlg::Des, 8, AesMode::Ecb) => dispatch_dec_ecb!(DesEcbDec, padding, data, key),
        (SymAlg::Des, 8, AesMode::Cbc) => dispatch_dec_cbc!(DesCbcDec, padding, data, key, iv),
        (SymAlg::Tdes, 16, AesMode::Ecb) => dispatch_dec_ecb!(Tdes2EcbDec, padding, data, key),
        (SymAlg::Tdes, 16, AesMode::Cbc) => dispatch_dec_cbc!(Tdes2CbcDec, padding, data, key, iv),
        (SymAlg::Tdes, 24, AesMode::Ecb) => dispatch_dec_ecb!(Tdes3EcbDec, padding, data, key),
        (SymAlg::Tdes, 24, AesMode::Cbc) => dispatch_dec_cbc!(Tdes3CbcDec, padding, data, key, iv),
        (SymAlg::Aes, n, _) => Err(AppError::bad_request(format!("AES 密钥长度须为 16/24/32 字节，实际 {}", n))),
        (SymAlg::Des, n, _) => Err(AppError::bad_request(format!("DES 密钥长度须为 8 字节，实际 {}", n))),
        (SymAlg::Tdes, n, _) => Err(AppError::bad_request(format!("3DES 密钥长度须为 16/24 字节，实际 {}", n))),
    }
}

/// 把可能是 Base64 或原始字符串的 key/iv 解码为字节
fn decode_key(s: &str, is_base64: bool) -> AppResult<Vec<u8>> {
    if is_base64 { b64().decode(s).map_err(|e| AppError::bad_request(format!("Base64 解码失败: {}", e))) }
    else { Ok(s.as_bytes().to_vec()) }
}

/// aesEncodeToString：加密 data（UTF-8 字节）→ Base64 字符串
/// 对齐官方 aesEncodeToBase64String(data, key, transformation, iv)
pub fn aes_encrypt_base64(data: &str, key: &str, transformation: &str, iv: &str, key_iv_b64: bool) -> AppResult<String> {
    let k = decode_key(key, key_iv_b64)?;
    let i = decode_key(iv, key_iv_b64)?;
    let out = sym_encrypt_bytes(SymAlg::Aes, data.as_bytes(), &k, &i, transformation)?;
    Ok(b64().encode(out))
}

/// aesEncodeToHex：加密 → hex
pub fn aes_encrypt_hex(data: &str, key: &str, transformation: &str, iv: &str, key_iv_b64: bool) -> AppResult<String> {
    let k = decode_key(key, key_iv_b64)?;
    let i = decode_key(iv, key_iv_b64)?;
    let out = sym_encrypt_bytes(SymAlg::Aes, data.as_bytes(), &k, &i, transformation)?;
    Ok(hex::encode(out))
}

/// aesDecodeToString：str 为 Base64 密文 → 解密 → UTF-8 字符串
pub fn aes_decrypt_base64_to_string(data: &str, key: &str, transformation: &str, iv: &str, key_iv_b64: bool) -> AppResult<String> {
    let k = decode_key(key, key_iv_b64)?;
    let i = decode_key(iv, key_iv_b64)?;
    let bytes = b64().decode(data).map_err(|e| AppError::bad_request(format!("密文 Base64 解码失败: {}", e)))?;
    let out = sym_decrypt_bytes(SymAlg::Aes, &bytes, &k, &i, transformation)?;
    Ok(String::from_utf8_lossy(&out).to_string())
}

/// aesDecodeToString：str 为 hex 密文 → 解密 → UTF-8 字符串
pub fn aes_decrypt_hex_to_string(data: &str, key: &str, transformation: &str, iv: &str, key_iv_b64: bool) -> AppResult<String> {
    let k = decode_key(key, key_iv_b64)?;
    let i = decode_key(iv, key_iv_b64)?;
    let bytes = hex::decode(data).map_err(|e| AppError::bad_request(format!("密文 hex 解码失败: {}", e)))?;
    let out = sym_decrypt_bytes(SymAlg::Aes, &bytes, &k, &i, transformation)?;
    Ok(String::from_utf8_lossy(&out).to_string())
}

// ---------------- 通用对称加密（createSymmetricCrypto / DES / 3DES） ----------------

/// 官方 isHex 判定：非空、长度为偶数、全部为 hex 字符（用于 decrypt(String) 自动识别）
fn is_hex(s: &str) -> bool {
    !s.is_empty()
        && s.len() % 2 == 0
        && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// createSymmetricCrypto(...).encryptBase64(data)
pub fn sym_encrypt_base64(data: &str, key: &str, transformation: &str, iv: &str) -> AppResult<String> {
    let out = sym_encrypt_bytes(SymAlg::Aes, data.as_bytes(), key.as_bytes(), iv.as_bytes(), transformation)?;
    Ok(b64().encode(out))
}

/// createSymmetricCrypto(...).encryptHex(data)
pub fn sym_encrypt_hex(data: &str, key: &str, transformation: &str, iv: &str) -> AppResult<String> {
    let out = sym_encrypt_bytes(SymAlg::Aes, data.as_bytes(), key.as_bytes(), iv.as_bytes(), transformation)?;
    Ok(hex::encode(out))
}

/// createSymmetricCrypto(...).encrypt(data) 的 String 化（官方 desEncodeToString 直接 String(bytes)）
pub fn sym_encrypt_raw_string(data: &str, key: &str, transformation: &str, iv: &str) -> AppResult<String> {
    let out = sym_encrypt_bytes(SymAlg::Aes, data.as_bytes(), key.as_bytes(), iv.as_bytes(), transformation)?;
    Ok(String::from_utf8_lossy(&out).to_string())
}

/// createSymmetricCrypto(...).decrypt(str)：密文自动识别 hex / Base64（对齐 SymmetricCryptoAndroid）
pub fn sym_decrypt_auto(data: &str, key: &str, transformation: &str, iv: &str) -> AppResult<Vec<u8>> {
    let bytes = if is_hex(data) {
        hex::decode(data).map_err(|e| AppError::bad_request(format!("密文 hex 解码失败: {}", e)))?
    } else {
        b64().decode(data).map_err(|e| AppError::bad_request(format!("密文 Base64 解码失败: {}", e)))?
    };
    sym_decrypt_bytes(SymAlg::Aes, &bytes, key.as_bytes(), iv.as_bytes(), transformation)
}

/// createSymmetricCrypto(...).decryptStr(str) → UTF-8 字符串
pub fn sym_decrypt_str(data: &str, key: &str, transformation: &str, iv: &str) -> AppResult<String> {
    let out = sym_decrypt_auto(data, key, transformation, iv)?;
    Ok(String::from_utf8_lossy(&out).to_string())
}

// ---------------- 官方遗留的 des*/tripleDES* 函数（底层同为 SymmetricCrypto） ----------------

/// desEncodeToString(data, key, transformation, iv)：String(encrypt(data))
pub fn des_encode_to_string(data: &str, key: &str, transformation: &str, iv: &str) -> AppResult<String> {
    sym_encrypt_raw_string(data, key, transformation, iv)
}

/// desEncodeToBase64String(data, key, transformation, iv)
pub fn des_encode_to_base64_string(data: &str, key: &str, transformation: &str, iv: &str) -> AppResult<String> {
    sym_encrypt_base64(data, key, transformation, iv)
}

/// desDecodeToString / desBase64DecodeToString(data, key, transformation, iv)：密文自动识别 hex/Base64
pub fn des_decode_to_string(data: &str, key: &str, transformation: &str, iv: &str) -> AppResult<String> {
    sym_decrypt_str(data, key, transformation, iv)
}

/// tripleDESEncodeBase64Str(data, key, mode, padding, iv)：transformation = DESede/mode/padding
pub fn tripledes_encode_base64(data: &str, key: &str, mode: &str, padding: &str, iv: &str) -> AppResult<String> {
    let t = format!("DESede/{}/{}", mode, padding);
    sym_encrypt_base64(data, key, &t, iv)
}

/// tripleDESDecodeStr(data, key, mode, padding, iv)
pub fn tripledes_decode_str(data: &str, key: &str, mode: &str, padding: &str, iv: &str) -> AppResult<String> {
    let t = format!("DESede/{}/{}", mode, padding);
    sym_decrypt_str(data, key, &t, iv)
}

/// tripleDESEncodeArgsBase64Str / tripleDESDecodeArgsBase64Str 的参数预处理：
/// key 为 Base64 编码，iv 为原始字符串字节（对齐官方）
pub fn tripledes_encode_args_base64(data: &str, key_b64: &str, mode: &str, padding: &str, iv: &str) -> AppResult<String> {
    let t = format!("DESede/{}/{}", mode, padding);
    let k = decode_key(key_b64, true)?;
    let out = sym_encrypt_bytes(SymAlg::Tdes, data.as_bytes(), &k, iv.as_bytes(), &t)?;
    Ok(b64().encode(out))
}

pub fn tripledes_decode_args_base64(data: &str, key_b64: &str, mode: &str, padding: &str, iv: &str) -> AppResult<String> {
    let t = format!("DESede/{}/{}", mode, padding);
    let k = decode_key(key_b64, true)?;
    let bytes = if is_hex(data) {
        hex::decode(data).map_err(|e| AppError::bad_request(format!("密文 hex 解码失败: {}", e)))?
    } else {
        b64().decode(data).map_err(|e| AppError::bad_request(format!("密文 Base64 解码失败: {}", e)))?
    };
    let out = sym_decrypt_bytes(SymAlg::Tdes, &bytes, &k, iv.as_bytes(), &t)?;
    Ok(String::from_utf8_lossy(&out).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_vectors() {
        assert_eq!(md5_encode(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_encode("abc"), "900150983cd24fb0d6963f7d28e17f72");
        // 官方 md5Encode16 = 32位结果 [8..24]
        assert_eq!(md5_encode16("abc"), "3cd24fb0d6963f7d");
    }

    #[test]
    fn digest_algorithms() {
        // FIPS 经典向量
        assert_eq!(digest_hex("abc", "sha1").unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(digest_hex("abc", "SHA-256").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(digest_hex("abc", "sha512").unwrap().len(), 128);
        assert_eq!(digest_base64("abc", "md5").unwrap(), b64().encode(Md5::digest(b"abc")));
    }

    #[test]
    fn hmac_vectors() {
        // RFC 2202: key="key", data="The quick brown fox jumps over the lazy dog"
        let data = "The quick brown fox jumps over the lazy dog";
        assert_eq!(hmac_hex(data, "md5", "key").unwrap(), "80070713463e7749b90c2dc24911e275");
        assert_eq!(hmac_hex(data, "sha1", "key").unwrap(), "de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9");
        assert_eq!(hmac_hex(data, "sha256", "key").unwrap(), "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8");
    }

    #[test]
    fn aes_ecb_pkcs7_roundtrip() {
        let key = "1234567890abcdef";
        let ct = aes_encrypt_base64("hello 世界", key, "AES/ECB/PKCS5Padding", "", false).unwrap();
        let pt = aes_decrypt_base64_to_string(&ct, key, "AES/ECB/PKCS5Padding", "", false).unwrap();
        assert_eq!(pt, "hello 世界");
    }

    #[test]
    fn aes_cbc_pkcs7_roundtrip() {
        let key = "1234567890abcdef";
        let iv = "fedcba0987654321";
        let ct = aes_encrypt_hex("secret-text", key, "AES/CBC/PKCS7Padding", iv, false).unwrap();
        let pt = aes_decrypt_hex_to_string(&ct, key, "AES/CBC/PKCS7Padding", iv, false).unwrap();
        assert_eq!(pt, "secret-text");
    }

    #[test]
    fn aes_nist_vector_cbc() {
        // NIST SP 800-38A CBC-AES128 标准向量
        let key = hex::decode("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        let iv = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();
        let pt = hex::decode("6bc1bee22e409f96e93d7e117393172a").unwrap();
        let ct = sym_encrypt_bytes(SymAlg::Aes, &pt, &key, &iv, "AES/CBC/NoPadding").unwrap();
        assert_eq!(hex::encode(ct), "7649abac8119b246cee98e9b12e9197d");
    }

    #[test]
    fn des_ecb_pkcs5_roundtrip() {
        // DES 8 字节密钥
        let ct = sym_encrypt_base64("hello", "12345678", "DES/ECB/PKCS5Padding", "").unwrap();
        assert_eq!(sym_decrypt_str(&ct, "12345678", "DES/ECB/PKCS5Padding", "").unwrap(), "hello");
    }

    #[test]
    fn des_cbc_pkcs5_roundtrip() {
        let ct = sym_encrypt_hex("数据", "12345678", "DES/CBC/PKCS5Padding", "87654321").unwrap();
        assert_eq!(sym_decrypt_str(&ct, "12345678", "DES/CBC/PKCS5Padding", "87654321").unwrap(), "数据");
    }

    #[test]
    fn tdes_16byte_key_matches_java_k1k2k1() {
        // Java DESede 16 字节密钥 = K1 K2 K1（EDE2），等价于 24 字节 K1K2K1（EDE3）
        let k16 = "1234567890abcdef";
        let k24 = format!("{}{}", k16, &k16[..8]);
        let c16 = sym_encrypt_base64("test-3des", k16, "DESede/ECB/PKCS5Padding", "").unwrap();
        let c24 = sym_encrypt_base64("test-3des", &k24, "DESede/ECB/PKCS5Padding", "").unwrap();
        assert_eq!(c16, c24);
        assert_eq!(sym_decrypt_str(&c24, &k24, "DESede/ECB/PKCS5Padding", "").unwrap(), "test-3des");
    }

    #[test]
    fn sym_decrypt_auto_hex_and_base64() {
        // 同一密文的 hex / Base64 两种输入都应可解（对齐 SymmetricCryptoAndroid.decrypt(String)）
        let key = "1234567890abcdef";
        let ct = sym_encrypt_hex("auto-detect", key, "AES/ECB/PKCS5Padding", "").unwrap();
        let ct_b64 = b64().encode(hex::decode(&ct).unwrap());
        assert_eq!(sym_decrypt_str(&ct, key, "AES/ECB/PKCS5Padding", "").unwrap(), "auto-detect");
        assert_eq!(sym_decrypt_str(&ct_b64, key, "AES/ECB/PKCS5Padding", "").unwrap(), "auto-detect");
        // isHex 语义：奇数长度 / 非 hex 字符 → 走 Base64
        assert!(!is_hex("abc"), "奇数长度不是 hex");
        assert!(is_hex("ABCDEF01"), "偶数长度 hex 合法");
    }

    #[test]
    fn tripledes_legacy_fns() {
        let c = tripledes_encode_base64("legacy", "1234567890abcdef12345678", "ECB", "PKCS5Padding", "").unwrap();
        assert_eq!(tripledes_decode_str(&c, "1234567890abcdef12345678", "ECB", "PKCS5Padding", "").unwrap(), "legacy");
        // ArgsBase64 变体：key 为 Base64
        let key_b64 = b64().encode(b"1234567890abcdef12345678");
        let c2 = tripledes_encode_args_base64("legacy-args", &key_b64, "ECB", "PKCS5Padding", "").unwrap();
        assert_eq!(tripledes_decode_args_base64(&c2, &key_b64, "ECB", "PKCS5Padding", "").unwrap(), "legacy-args");
    }

    #[test]
    fn des_wrong_key_len_rejected() {
        assert!(sym_encrypt_base64("x", "short", "DES/ECB/PKCS5Padding", "").is_err());
        assert!(sym_encrypt_base64("x", "1234567890abcdef", "DES/ECB/PKCS5Padding", "").is_err());
    }
}
