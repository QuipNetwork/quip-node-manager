#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

# Runs under `timeout` inside the validator, which already uses bash /dev/tcp
# for its healthcheck. Arguments are data, never interpolated into shell code.
export LC_ALL=C
probe_host=$1
probe_port=$2
probe_method=$3
probe_path=$4
probe_body=$5
exec 3<>"/dev/tcp/${probe_host}/${probe_port}"
# HTTP/1.0 asks for a close-delimited response without chunked transfer coding.
printf '%s %s HTTP/1.0\r\nHost: %s:%s\r\nContent-Type: application/json\r\nContent-Length: %d\r\nConnection: close\r\n\r\n%s' \
  "$probe_method" "$probe_path" "$probe_host" "$probe_port" "${#probe_body}" "$probe_body" >&3
cat <&3
