"""Regenerate the interop fixtures (signed by signxml/lxml, encrypted with
`cryptography`), independent of the Rust implementation:

    python3 -m venv v && v/bin/pip install signxml lxml cryptography
    v/bin/python gen.py
"""
import base64, os
from lxml import etree
from signxml import XMLSigner, methods
from cryptography.hazmat.primitives import serialization, hashes, padding as sympad
from cryptography.hazmat.primitives.asymmetric import padding
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography import x509

key = open("idp.key", "rb").read()
cert = open("idp.crt", "rb").read()

ASSERTION = """<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" ID="_a1" Version="2.0" IssueInstant="2024-01-01T00:00:00Z">
  <saml:Issuer>https://idp.example.com</saml:Issuer>
  <saml:Subject>
    <saml:NameID Format="urn:oasis:names:tc:SAML:1.1:nameid-format:unspecified">mona</saml:NameID>
    <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
      <saml:SubjectConfirmationData NotOnOrAfter="2099-01-01T00:00:00Z" Recipient="https://sp.example.com/saml/consume"/>
    </saml:SubjectConfirmation>
  </saml:Subject>
  <saml:Conditions NotBefore="2024-01-01T00:00:00Z" NotOnOrAfter="2099-01-01T00:00:00Z">
    <saml:AudienceRestriction><saml:Audience>https://sp.example.com</saml:Audience></saml:AudienceRestriction>
  </saml:Conditions>
  <saml:AuthnStatement AuthnInstant="2024-01-01T00:00:00Z" SessionIndex="s1"><saml:AuthnContext><saml:AuthnContextClassRef>urn:oasis:names:tc:SAML:2.0:ac:classes:Password</saml:AuthnContextClassRef></saml:AuthnContext></saml:AuthnStatement>
  <saml:AttributeStatement>
    <saml:Attribute Name="full_name"><saml:AttributeValue xsi:type="xs:string">Mona &amp; Lisa</saml:AttributeValue></saml:Attribute>
    <saml:Attribute Name="emails"><saml:AttributeValue xsi:type="xs:string">mona@example.com</saml:AttributeValue></saml:Attribute>
  </saml:AttributeStatement>
</saml:Assertion>"""

def response(inner):
    return ('<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" '
            'xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_r1" Version="2.0" '
            'IssueInstant="2024-01-01T00:00:00Z" Destination="https://sp.example.com/saml/consume">'
            '<saml:Issuer>https://idp.example.com</saml:Issuer>'
            '<samlp:Status><samlp:StatusCode Value="urn:oasis:names:tc:SAML:2.0:status:Success"/></samlp:Status>'
            + inner + '</samlp:Response>')

def sign(xml, c14n, ref_id):
    root = etree.fromstring(xml.encode())
    signer = XMLSigner(method=methods.enveloped, signature_algorithm="rsa-sha256",
                       digest_algorithm="sha256", c14n_algorithm=c14n)
    # signxml places the signature as the last child; SAML wants it after
    # Issuer, but verification does not care about the position.
    signed = signer.sign(root, key=key, cert=cert, reference_uri="#" + ref_id)
    return etree.tostring(signed).decode()

EXC = "http://www.w3.org/2001/10/xml-exc-c14n#"
INC = "http://www.w3.org/TR/2001/REC-xml-c14n-20010315"

# 1. Signed assertion (exclusive C14N) inside an unsigned response.
open("signed_assertion_exc.xml", "w").write(response(sign(ASSERTION, EXC, "_a1")))
# 2. Signed assertion (inclusive C14N 1.0).
open("signed_assertion_inc.xml", "w").write(response(sign(ASSERTION, INC, "_a1")))
# 3. Signed response (inclusive C14N), unsigned assertion.
open("signed_response.xml", "w").write(sign(response(ASSERTION), INC, "_r1"))

# 4. Encrypted signed assertion (AES-256-CBC + RSA-OAEP-MGF1P) for sp.crt.
plain = sign(ASSERTION, EXC, "_a1").encode()
sp = x509.load_pem_x509_certificate(open("sp.crt", "rb").read()).public_key()
cek, iv = os.urandom(32), os.urandom(16)
p = sympad.PKCS7(128).padder(); data = p.update(plain) + p.finalize()
enc = Cipher(algorithms.AES(cek), modes.CBC(iv)).encryptor()
ct = iv + enc.update(data) + enc.finalize()
ek = sp.encrypt(cek, padding.OAEP(mgf=padding.MGF1(hashes.SHA1()), algorithm=hashes.SHA1(), label=None))
b = lambda x: base64.b64encode(x).decode()
encrypted = (
    '<saml:EncryptedAssertion><xenc:EncryptedData xmlns:xenc="http://www.w3.org/2001/04/xmlenc#" '
    'Type="http://www.w3.org/2001/04/xmlenc#Element">'
    '<xenc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes256-cbc"/>'
    '<ds:KeyInfo xmlns:ds="http://www.w3.org/2000/09/xmldsig#"><xenc:EncryptedKey>'
    '<xenc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#rsa-oaep-mgf1p"/>'
    '<xenc:CipherData><xenc:CipherValue>' + b(ek) + '</xenc:CipherValue></xenc:CipherData>'
    '</xenc:EncryptedKey></ds:KeyInfo>'
    '<xenc:CipherData><xenc:CipherValue>' + b(ct) + '</xenc:CipherValue></xenc:CipherData>'
    '</xenc:EncryptedData></saml:EncryptedAssertion>')
open("encrypted_assertion.xml", "w").write(response(encrypted))
