// Copyright 2014 Google Inc. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use sodiumoxide::crypto::stream;
use hash::tree::HashRef;
use blob::{ChunkRef, Key};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};


pub struct PlainText(Vec<u8>);
pub struct CipherText(Vec<u8>);
pub struct CipherTextRef<'a>(&'a [u8]);


pub mod desc {
    pub use sodiumoxide::crypto::secretbox::xsalsa20poly1305::{KEYBYTES, Key, MACBYTES,
                                                               NONCEBYTES, Nonce};

    pub fn fixed_key_overhead() -> usize {
        // MAC for the plaintext being sealed + the footer.
        MACBYTES + footer_cipher_bytes()
    }

    pub fn footer_plain_bytes() -> usize {
        // Footer contains a Nonce and a LittleEndian u64.
        NONCEBYTES + 8
    }

    pub fn footer_cipher_bytes() -> usize {
        footer_plain_bytes() + MACBYTES
    }

    pub fn static_nonce() -> Nonce {
        Nonce::from_slice(&[255; NONCEBYTES]).unwrap()
    }
}

mod imp {
    pub use sodiumoxide::crypto::secretbox::xsalsa20poly1305::{gen_key, gen_nonce, open, seal};
}


fn wrap_key(key: desc::Key) -> Key {
    Key::XSalsa20Poly1305(key)
}


impl PlainText {
    pub fn new(bytes: Vec<u8>) -> PlainText {
        PlainText(bytes)
    }
    pub fn from_vec(mut bytes: &mut Vec<u8>) -> PlainText {
        let mut pt = PlainText(Vec::with_capacity(bytes.len()));
        pt.0.append(&mut bytes);
        pt
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn to_ciphertext(&self, nonce: &desc::Nonce, key: &desc::Key) -> CipherText {
        CipherText(imp::seal(&self.0, &nonce, &key))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

impl CipherText {
    pub fn new(ct: Vec<u8>) -> CipherText {
        CipherText(ct)
    }
    pub fn from(mut other_ct: &mut CipherText) -> CipherText {
        let mut ct = CipherText::new(Vec::with_capacity(other_ct.len()));
        ct.append(&mut other_ct);
        ct
    }
    pub fn append(&mut self, other: &mut CipherText) {
        self.0.append(&mut other.0);
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn empty_into(&mut self, out: &mut CipherText) {
        out.0.append(&mut self.0)
    }
    pub fn random_pad_upto(&mut self, final_size: usize) {
        let size = self.len();
        if final_size > size {
            self.0.resize(final_size, 0);
            let key = stream::salsa20::gen_key();
            let nonce = stream::salsa20::gen_nonce();
            stream::salsa20::stream_xor_inplace(&mut self.0[size..], &nonce, &key);
        }
    }
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }
    pub fn as_ref(&self) -> CipherTextRef {
        CipherTextRef(&self.0[..])
    }
}

impl<'a> CipherTextRef<'a> {
    pub fn new(bytes: &'a [u8]) -> CipherTextRef<'a> {
        CipherTextRef(bytes)
    }
    pub fn slice(&self, from: usize, to: usize) -> CipherTextRef<'a> {
        CipherTextRef(&self.0[from..to])
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn split_from_right(&self, len: usize) -> (CipherTextRef<'a>, CipherTextRef<'a>) {
        assert!(self.len() >= len);
        (self.slice(0, self.len() - len), self.slice(self.len() - len, self.len()))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn to_plaintext(&self, nonce: &desc::Nonce, key: &desc::Key) -> Result<PlainText, ()> {
        Ok(PlainText(try!(imp::open(&self.0, &nonce, &key))))
    }
}


pub struct RefKey {}

impl RefKey {
    pub fn seal(href: &mut HashRef, pt: PlainText) -> CipherText {
        // TODO(jos): WIP: Plug in encryption/crypto here.
        // Update cref with key.
        let key = imp::gen_key();
        href.persistent_ref.key = Some(wrap_key(key.clone()));

        let ct = pt.to_ciphertext(&desc::static_nonce(), &key);
        href.persistent_ref.length = ct.len();

        ct
    }

    pub fn unseal(hash: &[u8], cref: &ChunkRef, ct: CipherTextRef) -> Result<PlainText, ()> {
        assert!(ct.len() >= cref.offset + cref.length);
        let ct = ct.slice(cref.offset, cref.offset + cref.length);
        match cref.key {
            Some(Key::XSalsa20Poly1305(ref key)) => {
                Ok(try!(ct.to_plaintext(&desc::static_nonce(), &key)))
            }
            _ => panic!("Unknown blob key type"),
        }
    }
}

pub struct FixedKey {
    key: desc::Key,
}

impl FixedKey {
    pub fn new(key: desc::Key) -> FixedKey {
        FixedKey { key: key }
    }

    pub fn tie_knot(&self, pt: PlainText) -> CipherText {
        let nonce = imp::gen_nonce();
        let mut ct = pt.to_ciphertext(&nonce, &self.key);

        // Build footer from nonce and serialized ciphertext length.
        let ct_len = ct.len();
        let mut foot_pt = PlainText(nonce.as_ref().to_owned());
        foot_pt.0.write_u64::<LittleEndian>(ct_len as u64).unwrap();
        assert_eq!(foot_pt.len(), desc::footer_plain_bytes());

        // Tie the knot by sealing the nonce and ciphertext length.
        assert!(ct_len > desc::NONCEBYTES);
        let nonce = desc::Nonce::from_slice(&ct.0[ct_len - desc::NONCEBYTES..]).unwrap();
        ct.append(&mut foot_pt.to_ciphertext(&nonce, &self.key));

        // Return complete ciphertext.
        ct
    }

    fn untie_knot<'a>(&self, ct: CipherTextRef<'a>) -> Result<(CipherTextRef<'a>, PlainText), ()> {
        // Partial untie of knot: recover footer with nonce and ciphertext length.
        let foot_size = desc::footer_cipher_bytes();
        let (rest, foot_ct) = ct.split_from_right(foot_size);
        let nonce = desc::Nonce::from_slice(&rest.as_bytes()[rest.len() - desc::NONCEBYTES..])
            .unwrap();
        let foot_pt = try!(foot_ct.to_plaintext(&nonce, &self.key));

        let nonce = desc::Nonce::from_slice(&foot_pt.as_bytes()[..desc::NONCEBYTES]).unwrap();
        let ct_len = (&foot_pt.as_bytes()[desc::NONCEBYTES..]).read_u64::<LittleEndian>().unwrap();

        // Complete untie: recover plaintext.
        let (rest, ct) = rest.split_from_right(ct_len as usize);
        Ok((rest, try!(ct.to_plaintext(&nonce, &self.key))))
    }

    pub fn seal(&self, pt: PlainText) -> CipherText {
        // Seal with fixed key.
        self.tie_knot(pt)
    }

    pub fn unseal<'a, 'b>(&'a self,
                          ct: CipherTextRef<'b>)
                          -> Result<(CipherTextRef<'b>, PlainText), ()> {
        // Unseal with fixed key.
        self.untie_knot(ct)
    }
}
