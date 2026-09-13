// SPDX-License-Identifier: AGPL-3.0-or-later
function setPath(object, path, value) {
  const keys = path.split(".");
  let target = object;
  for (const key of keys.slice(0, -1)) {
    target[key] ??= {};
    target = target[key];
  }
  target[keys.at(-1)] = value;
}

export function applyPortEdits(config, controls, edits) {
  const next = structuredClone(config);
  for (const control of controls) {
    const edit = edits[control.id];
    const port = Number(edit?.host_port);
    if (
      !/^\d+$/.test(edit?.host_port ?? "") ||
      !Number.isInteger(port) ||
      port < 1 ||
      port > 65535
    ) {
      throw new Error(`${control.service} ${control.label}: choose a host port from 1 to 65535`);
    }
    setPath(next, control.port_path, port);
    if (!control.required || control.public_access_optional) {
      setPath(next, control.enabled_path, edit.enabled);
    }
  }
  return next;
}

export function renderPortControls(container, controls) {
  container.replaceChildren();
  let service;
  let group;
  for (const control of controls) {
    if (service !== control.service) {
      service = control.service;
      group = document.createElement("fieldset");
      group.className = "service-port-group";
      const legend = document.createElement("legend");
      legend.textContent = service;
      group.append(legend);
      container.append(group);
    }
    const row = document.createElement("div");
    row.className = "service-port-row";
    const label = document.createElement("label");
    const toggle = document.createElement("input");
    toggle.type = "checkbox";
    toggle.id = `publish-${control.id}`;
    toggle.checked = control.required || control.binding.enabled;
    toggle.disabled = control.required;
    const name = document.createElement("span");
    const portLabel =
      control.id === "miner_rest" && control.required ? "native" : control.container_port;
    name.textContent = `${control.label} (${portLabel}/${control.protocols.join("+")})`;
    if (control.required) {
      name.textContent += " (required in Native mode; this is a host port)";
    }
    label.append(toggle, name);
    const input = document.createElement("input");
    input.type = "number";
    input.id = `host-port-${control.id}`;
    input.min = "1";
    input.max = "65535";
    input.step = "1";
    input.required = true;
    input.value = String(control.binding.host_port);
    input.disabled = !toggle.checked;
    input.setAttribute("aria-label", `${service} ${control.label} host port`);
    toggle.addEventListener("change", () => {
      input.disabled = !toggle.checked;
    });
    row.append(label, input);
    // The access choice comes first: it decides who can reach the port, and
    // the port number below it only matters once that is settled.
    if (control.public_access_optional) {
      group.append(renderPublicAccessToggle(control));
    }
    group.append(row);
    if (control.id === "caddy_admin") {
      const note = document.createElement("p");
      note.className = "service-port-note";
      note.textContent =
        "The admin API can change Caddy configuration. Enable only on a trusted network.";
      group.append(note);
    }
  }
}

// Native mode publishes the validator RPC on the host so the host miner can
// reach it. This slider chooses the bind address: loopback keeps it to this
// computer, all interfaces opens it to the network.
function renderPublicAccessToggle(control) {
  const access = document.createElement("div");
  access.className = "service-port-public-access";
  const row = document.createElement("label");
  row.className = "toggle-row";
  const toggle = document.createElement("span");
  toggle.className = "toggle-switch";
  const checkbox = document.createElement("input");
  checkbox.type = "checkbox";
  checkbox.id = `public-${control.id}`;
  checkbox.checked = control.binding.enabled;
  const slider = document.createElement("span");
  slider.className = "toggle-slider";
  toggle.append(checkbox, slider);
  const caption = document.createElement("span");
  caption.textContent = "Allow public access";
  row.append(toggle, caption);
  const note = document.createElement("p");
  note.className = "service-port-note";
  note.textContent =
    "On: the port listens on every network interface, so other machines can " +
    "reach it. Off: it listens on 127.0.0.1 only, so only programs on this " +
    "computer can reach it. The native miner reaches it either way.";
  access.append(row, note);
  return access;
}

export function collectPortEdits(container, controls) {
  return Object.fromEntries(
    controls.map(({ id, public_access_optional }) => [
      id,
      {
        enabled: container.querySelector(
          public_access_optional ? `#public-${id}` : `#publish-${id}`,
        ).checked,
        host_port: container.querySelector(`#host-port-${id}`).value,
      },
    ]),
  );
}
