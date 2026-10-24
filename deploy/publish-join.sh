#!/usr/bin/env bash
#
# Regenerate the signed join descriptor clients fetch when they have no
# connection link. The descriptor names the public seed(s) and, optionally,
# the attestor keys that may fund domain claims.
#
# The signing key lives outside the repository by default. The script never
# prints the secret; it only prints the public signer key, which is what
# clients pin with --join-signer / PWB_JOIN_SIGNER.
#
#   deploy/publish-join.sh \
#     --seed 'seed.example.com:7921/<hex_pubkey>' \
#     --anchor <attestor_hex_pubkey> \
#     --expires-in 180
#
# To withdraw the published seed list in an emergency, sign a zero-seed
# descriptor instead:
#
#   deploy/publish-join.sh --withdraw
#
# Run with --help for all options. See docs/runbooks/publish-a-seed.md.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

EXPIRE_DAYS=180
EXPIRES_AT=""
KEY_PATH="${PWB_JOIN_KEY:-${HOME:-}/.pwb/join-release-identity.json}"
OUTPUT="$REPO_ROOT/deploy/join.json"
NODE_BIN="${PWB_NODE:-}"
FORCE_KEY_IN_REPO=0
WITHDRAW=0
SEEDS=()
ANCHORS=()
SEEDS_FILE=""

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

note() {
    printf '==> %s\n' "$*" >&2
}

usage() {
    cat <<'EOF'
Regenerate and verify the signed PlainWeb join descriptor.

Usage:
  publish-join.sh --seed <link> [--seed <link> ...] [options]
  publish-join.sh --withdraw [options]

Required (unless --withdraw or --seeds-file is used):
  -s, --seed <link>        Seed address, repeatable. Accepts either
                           'host:port/<64-hex pubkey>' or the
                           'plain://nodes.plain?connect=host:port/<key>' link
                           printed by the seed at startup.
      --seeds-file <path>  Read seed links from a file (one per line, blank
                           lines and '#' comments ignored).

Options:
      --withdraw           Sign a descriptor that intentionally lists no seeds.
                           Clients with --join-required exit instead of
                           dialling a revoked host; flagless clients fall back
                           to LAN discovery. Cannot be combined with --seed.
  -a, --anchor <hex>       Pinned funding attestor, repeatable. Without any
                           anchors, clients keep the live-peer funding rule.
  -e, --expires-in <days>  Descriptor lifetime in days (default: 180).
      --expires-at <epoch> Use an explicit unix expiry instead of a duration.
      --no-expiry          Publish without an expiry (rotate manually).
  -k, --identity <path>    Signing identity (default: $PWB_JOIN_KEY, else
                           ~/.pwb/join-release-identity.json). Created with
                           0600 on first run.
  -o, --output <path>      Output path (default: <repo>/deploy/join.json).
  -n, --node <path>        pwb-node binary (default: $PWB_NODE, then PATH,
                           then <repo>/target/release/pwb-node, then the
                           debug build).
      --force-key-in-repo  Allow the signing key inside the repository. Not
                           recommended: a committed key lets anyone rewrite
                           the published seed list.
  -h, --help               Show this help.

The script writes the descriptor atomically, verifies its signature and
expiry with 'pwb-node --verify-join', and prints the public signer key to
publish out of band. A withdrawal is published exactly like a seed list:
review it, commit or upload it to the same URL, and keep the signing key.
EOF
}

trim() {
    printf '%s' "$1" | tr -d '\r' | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'
}

normalize_seed() {
    local spec
    spec="$(trim "$1")"
    spec="${spec#plain://nodes.plain?connect=}"
    spec="$(trim "$spec")"
    [[ -n $spec ]] || die "empty seed link"
    if [[ ! $spec =~ ^(\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9._-]+):[0-9]{1,5}/[0-9A-Fa-f]{64}$ ]]; then
        die "not a seed link: '$1' (expected host:port/<64-hex pubkey> or a plain:// connection link)"
    fi
    printf '%s\n' "$spec"
}

normalize_anchor() {
    local key
    key="$(trim "$1")"
    if [[ ! $key =~ ^[0-9A-Fa-f]{64}$ ]]; then
        die "not an attestor key: '$1' (expected 64 hex characters)"
    fi
    printf '%s\n' "$key"
}

while (($#)); do
    case "$1" in
    -s | --seed)
        [[ $# -ge 2 ]] || die "$1 requires a value"
        SEEDS+=("$2")
        shift 2
        ;;
    --seed=*)
        SEEDS+=("${1#*=}")
        shift
        ;;
    --seeds-file)
        [[ $# -ge 2 ]] || die "$1 requires a path"
        SEEDS_FILE="$2"
        shift 2
        ;;
    --withdraw)
        WITHDRAW=1
        shift
        ;;
    -a | --anchor)
        [[ $# -ge 2 ]] || die "$1 requires a value"
        ANCHORS+=("$2")
        shift 2
        ;;
    -e | --expires-in)
        [[ $# -ge 2 ]] || die "$1 requires a number of days"
        [[ $2 =~ ^[0-9]+$ ]] || die "--expires-in must be a whole number of days"
        EXPIRE_DAYS="$2"
        shift 2
        ;;
    --expires-at)
        [[ $# -ge 2 ]] || die "$1 requires a unix timestamp"
        [[ $2 =~ ^[0-9]+$ ]] || die "--expires-at must be a unix timestamp"
        EXPIRES_AT="$2"
        shift 2
        ;;
    --no-expiry)
        EXPIRES_AT="none"
        shift
        ;;
    -k | --identity)
        [[ $# -ge 2 ]] || die "$1 requires a path"
        KEY_PATH="$2"
        shift 2
        ;;
    -o | --output)
        [[ $# -ge 2 ]] || die "$1 requires a path"
        OUTPUT="$2"
        shift 2
        ;;
    -n | --node)
        [[ $# -ge 2 ]] || die "$1 requires a path"
        NODE_BIN="$2"
        shift 2
        ;;
    --force-key-in-repo)
        FORCE_KEY_IN_REPO=1
        shift
        ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        die "unknown argument '$1' (try --help)"
        ;;
    esac
done

if [[ -n $SEEDS_FILE ]]; then
    [[ -r $SEEDS_FILE ]] || die "cannot read seeds file '$SEEDS_FILE'"
    while IFS= read -r line || [[ -n $line ]]; do
        line="$(trim "$line")"
        [[ -z $line || ${line:0:1} == "#" ]] && continue
        SEEDS+=("$line")
    done <"$SEEDS_FILE"
fi

if ((WITHDRAW)); then
    ((${#SEEDS[@]} == 0)) ||
        die "--withdraw signs a zero-seed descriptor; remove --seed/--seeds-file"
else
    ((${#SEEDS[@]} > 0)) ||
        die "no seeds given; pass --seed <host:port>/<hex_pubkey> or --seeds-file <path>"
fi

if [[ -z $NODE_BIN ]]; then
    if command -v pwb-node >/dev/null 2>&1; then
        NODE_BIN="$(command -v pwb-node)"
    elif [[ -x $REPO_ROOT/target/release/pwb-node ]]; then
        NODE_BIN="$REPO_ROOT/target/release/pwb-node"
    elif [[ -x $REPO_ROOT/target/debug/pwb-node ]]; then
        NODE_BIN="$REPO_ROOT/target/debug/pwb-node"
        note "Using the debug build at $NODE_BIN"
    else
        die "pwb-node not found; build it (cargo build --release -p pwb-node) or pass --node <path>"
    fi
fi
[[ -x $NODE_BIN ]] || die "pwb-node at '$NODE_BIN' is not executable"
"$NODE_BIN" --help 2>/dev/null | grep -q -- '--verify-join' ||
    die "'$NODE_BIN' predates --verify-join; rebuild the node binary"
if ((WITHDRAW)); then
    "$NODE_BIN" --help 2>/dev/null | grep -q -- '--withdraw-join' ||
        die "'$NODE_BIN' predates --withdraw-join; rebuild the node binary"
fi

NORMALIZED_SEEDS=()
for seed in "${SEEDS[@]}"; do
    NORMALIZED_SEEDS+=("$(normalize_seed "$seed")")
done
NORMALIZED_ANCHORS=()
for anchor in "${ANCHORS[@]}"; do
    NORMALIZED_ANCHORS+=("$(normalize_anchor "$anchor")")
done

case "$EXPIRES_AT" in
none) ;;
"")
    [[ $EXPIRE_DAYS =~ ^[1-9][0-9]*$ ]] ||
        die "--expires-in must be at least 1 day (use --no-expiry to omit it)"
    EXPIRES_AT=$(($(date -u +%s) + EXPIRE_DAYS * 86400))
    ;;
*)
    if (($(date -u +%s) >= EXPIRES_AT)); then
        die "--expires-at $EXPIRES_AT is in the past; clients would reject the descriptor"
    fi
    ;;
esac

case "$KEY_PATH" in
"~/"*) KEY_PATH="$HOME/${KEY_PATH#\~/}" ;;
esac
case "$KEY_PATH" in
/*) ;;
*) KEY_PATH="$PWD/$KEY_PATH" ;;
esac
KEY_DIR="$(dirname "$KEY_PATH")"
if [[ ! -d $KEY_DIR ]]; then
    mkdir -p -m 700 "$KEY_DIR" || die "cannot create '$KEY_DIR'"
fi
KEY_DIR="$(cd "$KEY_DIR" && pwd -P)"
KEY_PATH="$KEY_DIR/$(basename "$KEY_PATH")"
case "$KEY_PATH" in
"$REPO_ROOT"/*)
    if ((FORCE_KEY_IN_REPO == 0)); then
        die "refusing to keep a signing key inside the repository ($KEY_PATH); choose --identity outside it or pass --force-key-in-repo"
    fi
    note "WARNING: signing key is inside the repository; do not commit it"
    ;;
esac

mkdir -p "$(dirname "$OUTPUT")" || die "cannot create the output directory"
OUTPUT="$(cd "$(dirname "$OUTPUT")" && pwd -P)/$(basename "$OUTPUT")"
TMP_OUT="$(mktemp "${OUTPUT}.tmp.XXXXXX")" || die "cannot create a temporary file beside '$OUTPUT'"
LOG="$(mktemp)" || die "cannot create a temporary log file"
cleanup() {
    rm -f "$TMP_OUT" "$LOG"
}
trap cleanup EXIT

ARGS=(--identity "$KEY_PATH" --write-join "$TMP_OUT")
if ((WITHDRAW)); then
    ARGS+=(--withdraw-join)
fi
for seed in "${NORMALIZED_SEEDS[@]}"; do
    ARGS+=(--join-seed "$seed")
done
for anchor in "${NORMALIZED_ANCHORS[@]}"; do
    ARGS+=(--trust-anchor "$anchor")
done
if [[ $EXPIRES_AT != none ]]; then
    ARGS+=(--join-expires "$EXPIRES_AT")
fi

if [[ ! -f $KEY_PATH ]]; then
    note "Creating a release signing identity at $KEY_PATH"
fi
if ! env -u PWB_BOOTSTRAP -u PWB_TRUST_ANCHORS -u PWB_JOIN -u PWB_JOIN_SIGNER \
    -u PWB_DATA -u PWB_IDENTITY \
    "$NODE_BIN" "${ARGS[@]}" >"$LOG" 2>&1; then
    cat "$LOG" >&2
    die "pwb-node --write-join failed"
fi
cat "$LOG"
chmod 600 "$KEY_PATH"

SIGNER="$(sed -n 's/^\[Join\] Signer public key: //p' "$LOG" | tail -n 1)"
[[ $SIGNER =~ ^[0-9A-Fa-f]{64}$ ]] || die "could not read the signer key from pwb-node output"
if grep -q 'secret_key_hex' "$TMP_OUT"; then
    die "the descriptor contains a secret key; refusing to publish it"
fi

# Independent check: parse it back the way a client does, enforce the
# signature, and confirm the public signer matches what was just printed.
if ! env -u PWB_JOIN_SIGNER "$NODE_BIN" --verify-join "$TMP_OUT" \
    --join-signer "$SIGNER" --require-join-signature >&2; then
    die "the descriptor written to '$TMP_OUT' does not verify"
fi

chmod 644 "$TMP_OUT"
mv -f "$TMP_OUT" "$OUTPUT" || die "cannot move the descriptor into '$OUTPUT'"

if [[ $EXPIRES_AT == none ]]; then
    EXPIRY_SUMMARY="never (rotate manually)"
else
    EXPIRY_SUMMARY="epoch $EXPIRES_AT (in $(( (EXPIRES_AT - $(date -u +%s)) / 86400 )) day(s))"
fi

if ((WITHDRAW)); then
    printf '\nWrote %s (signed withdrawal)\n' "$OUTPUT"
    printf '  mode   : withdrawal (zero seeds by design)\n'
else
    printf '\nWrote %s\n' "$OUTPUT"
    printf '  seeds  : %s\n' "${#NORMALIZED_SEEDS[@]}"
fi
printf '  signer : %s\n' "$SIGNER"
printf '  anchors: %s\n' "${#NORMALIZED_ANCHORS[@]}"
printf '  expires: %s\n' "$EXPIRY_SUMMARY"
if ((${#NORMALIZED_ANCHORS[@]} == 0)); then
    printf '\nNote: no --trust-anchor given, so clients keep the live-peer funding rule.\n'
fi
printf '\nNext steps:\n'
printf '  1. Review and publish %s (clients fetch the default URL in pwb-node).\n' "$OUTPUT"
printf '  2. Publish the signer key out of band: %s\n' "$SIGNER"
printf '     Clients pin it with --join-signer <hex> / PWB_JOIN_SIGNER.\n'
if ((WITHDRAW)); then
    printf '  3. Clients with --join-required will now exit by design.\n'
    printf '     To restore service, rerun without --withdraw once a healthy seed exists.\n'
else
    printf '  3. Verify any published copy with:\n'
    printf '     pwb-node --verify-join <url> --join-signer %s --require-join-signature\n' "$SIGNER"
fi
