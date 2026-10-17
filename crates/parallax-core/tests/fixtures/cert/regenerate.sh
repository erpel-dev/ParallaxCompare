#!/bin/sh
# Test-only certificates and keys, made with OpenSSL (and Python's cryptography package for the OpenSSH key). None of these keys
# protect anything. Dates are fixed so the fixtures are reproducible apart from keys and signatures.
set -e
cd "$(dirname "$0")"
rm -rf work && mkdir work
mkdir -p work/ca work/out && : > work/ca/index.txt && echo 01 > work/ca/crlnumber
cat > work/ca.cnf <<'X'
[ca]
default_ca = test
[test]
database = work/ca/index.txt
crlnumber = work/ca/crlnumber
serial = work/ca/serial
new_certs_dir = work/out
default_md = sha256
default_crl_days = 30
policy = any
unique_subject = no
copy_extensions = copy
[any]
countryName = optional
organizationName = optional
commonName = supplied
[root]
basicConstraints = critical,CA:TRUE,pathlen:0
keyUsage = critical,keyCertSign,cRLSign
subjectKeyIdentifier = hash
[leaf]
keyUsage = critical,digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth,clientAuth
basicConstraints = CA:FALSE
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid
X
# OpenSSL 3.0 has no -not_before for req/x509; `openssl ca` takes fixed dates.
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out ca.key
openssl req -new -key ca.key -subj "/C=DE/O=Parallax Test/CN=Parallax Test Root CA" -out work/ca.csr
echo 01 > work/ca/serial
openssl ca -batch -config work/ca.cnf -selfsign -keyfile ca.key -in work/ca.csr -extensions root -notext \
  -startdate 20260101000000Z -enddate 20360101000000Z -out ca.pem
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out server.key
openssl req -new -key server.key -subj "/C=DE/O=Parallax Test/CN=shop.example.com" \
  -addext "subjectAltName=DNS:shop.example.com,DNS:www.shop.example.com,IP:192.0.2.10,email:admin@example.com" -out server.csr
sign() { echo "$1" > work/ca/serial; openssl ca -batch -config work/ca.cnf -keyfile ca.key -cert ca.pem -in server.csr \
  -extensions leaf -notext -startdate "$2" -enddate "$3" -out "$4"; }
sign 1001 20260301000000Z 20270301000000Z server.pem
sign 1002 20270201000000Z 20280201000000Z server-renewed.pem
openssl x509 -in server.pem -outform DER -out server.der
openssl x509 -in server.pem -text > server-with-text.pem
cat server.pem ca.pem > chain.pem
# A CRL revoking the first server certificate.
openssl ca -batch -config work/ca.cnf -keyfile ca.key -cert ca.pem -revoke server.pem -crl_reason keyCompromise
openssl ca -batch -config work/ca.cnf -gencrl -keyfile ca.key -cert ca.pem -out ca.crl.pem
# Keys in their usual containers.
openssl pkey -in server.key -pubout -out server.pub.pem
openssl genpkey -algorithm ED25519 -out ed25519.key
openssl ecparam -name secp384r1 -genkey -noout -out ec-sec1.key
openssl pkey -in server.key -traditional -out rsa-pkcs1.key
openssl pkcs8 -topk8 -in server.key -v2 aes-256-cbc -passout pass:secret -out encrypted.key
# OpenSSH format, as ssh-keygen -t ed25519 -N "" writes it (Python cryptography, as ssh-keygen may be missing).
python3 -c 'from cryptography.hazmat.primitives.asymmetric import ed25519; from cryptography.hazmat.primitives import serialization as s
open("openssh.key", "wb").write(ed25519.Ed25519PrivateKey.generate().private_bytes(s.Encoding.PEM, s.PrivateFormat.OpenSSH, s.NoEncryption()))'

# PKCS#12: with a password, with the empty password, legacy algorithms, and without encryption.
openssl pkcs12 -export -in server.pem -inkey server.key -certfile ca.pem -name shop -passout pass:secret -out bundle.p12
openssl pkcs12 -export -in server.pem -inkey server.key -certfile ca.pem -name shop -passout pass: -out empty-password.p12
openssl pkcs12 -export -legacy -in server.pem -inkey server.key -name shop -passout pass:secret -out legacy.p12
openssl pkcs12 -export -in server.pem -inkey server.key -name shop -keypbe NONE -certpbe NONE -nomac -passout pass: -out unencrypted.p12
rm -rf work
