# Test fixtures

`github-app-rsa-test.pem` / `github-app-rsa-test.pub.pem`: a throwaway RSA-2048
key pair generated with `openssl genrsa -traditional 2048` exclusively for unit
tests of the GitHub App JWT minting path. It is not registered with any GitHub
App, grants no access anywhere, and exists only so tests can verify that
signing, claim layout, and redaction behave as specified. Rotation: regenerate
at will; nothing outside `mod tests` reads these files.
