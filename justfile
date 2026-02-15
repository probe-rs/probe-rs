# use PowerShell instead of sh:
set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

all: check check-fmt clippy test

clippy:
  cargo clippy

fmt:
  cargo fmt

check-fmt:
  cargo fmt --all -- --check

check:
  cargo check
  cargo check --no-default-features
  cargo check --all-features

test:
  cargo nextest run --all-features

msrv:
  cargo +1.89 check --all-features
