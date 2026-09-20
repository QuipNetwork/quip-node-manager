// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect } from "bun:test";
import { applyPortEdits, requiredNote } from "./service-ports.js";

const rpc = {
  id: "validator_rpc",
  service: "quip-validator",
  label: "RPC",
  required: false,
  port_path: "validator_rpc_port",
  enabled_path: "validator_rpc_enabled",
};
const metrics = {
  id: "validator_metrics",
  service: "quip-validator",
  label: "Metrics",
  required: false,
  port_path: "service_ports.validator_metrics.host_port",
  enabled_path: "service_ports.validator_metrics.enabled",
};

test("public port edits preserve unrelated settings and use the selected host ports", () => {
  const base = {
    validator_rpc_port: 9944,
    validator_rpc_enabled: false,
    node_name: "test",
    service_ports: { validator_metrics: { enabled: false, host_port: 9615 } },
  };
  const edited = applyPortEdits(base, [rpc, metrics], {
    validator_rpc: { enabled: true, host_port: "29944" },
    validator_metrics: { enabled: true, host_port: "29615" },
  });
  expect(edited).toEqual({
    validator_rpc_port: 29944,
    validator_rpc_enabled: true,
    node_name: "test",
    service_ports: { validator_metrics: { enabled: true, host_port: 29615 } },
  });
  expect(base.validator_rpc_port).toBe(9944);
});

test("Native required RPC keeps the saved Docker enable preference", () => {
  const edited = applyPortEdits({ validator_rpc_enabled: false }, [{ ...rpc, required: true }], {
    validator_rpc: { enabled: true, host_port: "65535" },
  });
  expect(edited.validator_rpc_port).toBe(65535);
  expect(edited.validator_rpc_enabled).toBe(false);
});

test("invalid ports are rejected instead of silently falling back to 9944", () => {
  for (const value of ["", "0", "65536", "-1", "1.5", "9944x"]) {
    expect(() =>
      applyPortEdits({}, [rpc], {
        validator_rpc: { enabled: true, host_port: value },
      }),
    ).toThrow("quip-validator RPC");
  }
});

test("Native RPC public access is an independent explicit choice", () => {
  const control = { ...rpc, required: true, public_access_optional: true };
  for (const enabled of [false, true]) {
    const edited = applyPortEdits({ validator_rpc_enabled: !enabled }, [control], {
      validator_rpc: { enabled, host_port: "29944" },
    });
    expect(edited.validator_rpc_enabled).toBe(enabled);
    expect(edited.validator_rpc_port).toBe(29944);
  }
});

test("Native REST edits use the host listener field", () => {
  const control = {
    ...metrics,
    id: "miner_rest",
    required: true,
    port_path: "rest_insecure_port",
    enabled_path: "service_ports.miner_rest.enabled",
  };
  const edited = applyPortEdits(
    { service_ports: { miner_rest: { enabled: false, host_port: 8086 } } },
    [control],
    { miner_rest: { enabled: true, host_port: "50000" } },
  );
  expect(edited.rest_insecure_port).toBe(50000);
  expect(edited.service_ports.miner_rest).toEqual({ enabled: false, host_port: 8086 });
});

test("required rows explain why they cannot be turned off", () => {
  expect(requiredNote({ id: "log_collector", required: true })).toBe(
    " (always on; listens on 127.0.0.1 only)",
  );
  expect(requiredNote({ id: "validator_rpc", required: true })).toBe(
    " (required in Native mode; this is a host port)",
  );
  expect(requiredNote({ id: "validator_rpc", required: false })).toBe("");
});

test("editing the collector row changes only its host port", () => {
  const collector = {
    id: "log_collector",
    service: "quip-dashboard",
    label: "Log collector",
    required: true,
    public_access_optional: false,
    port_path: "service_ports.log_collector.host_port",
    enabled_path: "service_ports.log_collector.enabled",
  };
  const base = { service_ports: { log_collector: { enabled: true, host_port: 5514 } } };
  const edited = applyPortEdits(base, [collector], {
    log_collector: { enabled: false, host_port: "15514" },
  });
  expect(edited.service_ports.log_collector).toEqual({ enabled: true, host_port: 15514 });
});
