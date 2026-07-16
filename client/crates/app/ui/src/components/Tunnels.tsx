import { openExternal } from "../api";
import type { Status, LocalService, CloudRoute } from "../types";

interface Props {
  status: Status;
  onError: (message: string) => void;
}

function open(url: string | null | undefined, onError: (m: string) => void) {
  if (!url) return;
  openExternal(url).catch((e) => onError(String(e)));
}

/** Local + Cloud tunnels, each openable in the default browser. */
export default function Tunnels({ status, onError }: Props) {
  const local: LocalService[] = status.local_services ?? [];
  const cloud: CloudRoute[] = status.cloud_routes ?? [];

  return (
    <section className="section">
      <div className="section-head">
        <h2>Tunnels</h2>
        <p className="section-note">
          Give any local app a real name instead of juggling ports.
        </p>
      </div>

      <h3>Local tunnels</h3>
      <div className="status-row">
        <span className={"dot" + (status.overlay_active ? " ok" : "")} />
        {status.overlay_active ? "overlay active" : "overlay inactive"}
      </div>
      {local.length > 0 ? (
        <table>
          <thead>
            <tr>
              <th>domain</th>
              <th>real addr</th>
              <th>port</th>
              <th>pid</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {local.map((s, i) => (
              <tr key={i}>
                <td>
                  <strong>{s.domain}</strong>
                </td>
                <td>{s.real_addr ?? "-"}</td>
                <td>{s.service_port ?? "-"}</td>
                <td>{s.pid ?? "-"}</td>
                <td>
                  {s.link_url && (
                    <button
                      className="button ghost"
                      onClick={() => open(s.link_url, onError)}
                    >
                      Open
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <p className="empty">
          no local tunnels yet — run an example below to see one appear
        </p>
      )}

      <h3 style={{ marginTop: 22 }}>Cloud tunnels</h3>
      <div className="status-row">
        <span className={"dot" + (status.cloud_connected ? " ok" : "")} />
        {status.cloud_connected ? "connected" : "disconnected"}
        {status.cloud_plan ? ` · ${status.cloud_plan}` : ""}
        {status.cloud_error ? ` · ${status.cloud_error}` : ""}
      </div>
      {cloud.length > 0 ? (
        <table>
          <thead>
            <tr>
              <th>domain</th>
              <th>status</th>
              <th>port</th>
              <th>pid</th>
            </tr>
          </thead>
          <tbody>
            {cloud.map((r, i) => (
              <tr key={i}>
                <td>
                  <strong>{r.domain}</strong>
                </td>
                <td>{r.status ?? "published"}</td>
                <td>{r.port ?? "-"}</td>
                <td>{r.pid ?? "-"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : !status.auth_authenticated ? (
        <p className="empty">
          no Cloud tunnels — you are not logged in (run{" "}
          <code>portzero login</code>)
        </p>
      ) : (
        <p className="empty">no Cloud tunnels</p>
      )}
    </section>
  );
}
