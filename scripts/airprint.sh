#!/bin/sh -e
#
# Advertise the LX-D02 as an AirPrint / IPP Everywhere printer.
#
# Wraps macOS's own `ippeveprinter`, which provides the IPP server and the
# Bonjour advertisement; every job it accepts is handed to
# `printable ipp-command`. See docs/AIRPRINT.md.
#
#   scripts/airprint.sh                        # print for real
#   AIRPRINT_ARGS="--preview /tmp/job.png" \
#     scripts/airprint.sh                      # render instead, no paper
#
# Environment:
#   AIRPRINT_NAME   service name shown to clients (default "printa-ble")
#   AIRPRINT_PORT   TCP port to listen on       (default 8631)
#   AIRPRINT_ARGS   extra flags for `printable ipp-command`, e.g.
#                   "--density 2", "--preview /tmp/job.png", "--device LX-D02"
#   AIRPRINT_SPOOL  directory to keep spooled jobs and verbose logs in; set
#                   this when debugging a client, then inspect the .urf files
#   AIRPRINT_CONF   printer attributes file (default airprint-receipt.conf,
#                   which declares 48 mm continuous roll at 203 dpi)
#   PRINTABLE       path to the printable binary (default: first on PATH)
#   IPPEVE          path to ippeveprinter (default: newest one found)

NAME=${AIRPRINT_NAME:-printa-ble}
PORT=${AIRPRINT_PORT:-8631}
PRINTABLE=${PRINTABLE:-$(command -v printable || true)}

if [ -z "$PRINTABLE" ] || [ ! -x "$PRINTABLE" ]; then
	echo "airprint.sh: cannot find the 'printable' binary." >&2
	echo "  build it with 'cargo build --release' then re-run as:" >&2
	echo "  PRINTABLE=./target/release/printable $0" >&2
	exit 1
fi

# Prefer a Homebrew CUPS over the system one. macOS 26 still ships CUPS 2.3.4
# (2020), whose ippeveprinter rejects the Create-Job request iOS sends with
# "Unexpected document data following request" — the same operation from
# ipptool or CUPS succeeds, so it is that release mishandling iOS's HTTP
# framing, not a fault in the request. Printing from a Mac works on either.
IPPEVE=${IPPEVE:-}
if [ -z "$IPPEVE" ]; then
	for candidate in \
		/opt/homebrew/opt/cups/bin/ippeveprinter \
		/usr/local/opt/cups/bin/ippeveprinter \
		"$(command -v ippeveprinter 2>/dev/null || true)"; do
		if [ -n "$candidate" ] && [ -x "$candidate" ]; then
			IPPEVE=$candidate
			break
		fi
	done
fi

if [ -z "$IPPEVE" ]; then
	echo "airprint.sh: ippeveprinter not found (it ships with CUPS on macOS)." >&2
	exit 1
fi

# ippeveprinter's -c takes a bare command path with no arguments, so the flags
# have to be baked into a shim.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT INT TERM
cat >"$WORK/print-command" <<EOF
#!/bin/sh
# ippeveprinter passes the spooled job as \$1. Forwarding "\$@" is mandatory:
# without it the command reads an empty stdin and every job aborts.
exec "$PRINTABLE" ipp-command $AIRPRINT_ARGS "\$@"
EOF
chmod +x "$WORK/print-command"

echo "Advertising \"$NAME\" on port $PORT using $IPPEVE (Ctrl-C to stop)."
echo "Add it as a CUPS queue with:"
echo "  lpadmin -p printable -E -v ipp://localhost:$PORT/ipp/print -m everywhere"

# -f image/urf     accept only Apple Raster, so the *client* rasterises and we
#                  need no PDF renderer.
# -r _universal    the AirPrint subtype. The leading underscore is required —
#                  without it the service registers on plain _ipp._tcp only,
#                  which macOS still finds but iOS does not.
# Printer attributes: 48 mm continuous roll at 203 dpi. Without this the
# client lays the document out for US Letter and 5100 px has to be squeezed
# onto 384 px of paper. Note that -a and -f are mutually exclusive, so the
# supported formats live in the conf too.
CONF=${AIRPRINT_CONF:-$(dirname "$0")/airprint-receipt.conf}
if [ ! -f "$CONF" ]; then
	echo "airprint.sh: attributes file not found: $CONF" >&2
	exit 1
fi

if [ -n "${AIRPRINT_SPOOL:-}" ]; then
	mkdir -p "$AIRPRINT_SPOOL"
	echo "Keeping spooled jobs in $AIRPRINT_SPOOL"
	set -- -k -d "$AIRPRINT_SPOOL" -vv
else
	set --
fi

exec "$IPPEVE" \
	-a "$CONF" \
	-r _universal \
	-p "$PORT" \
	-c "$WORK/print-command" \
	"$@" \
	"$NAME"
