# Cloud tunnels in the local UI vs app.portzero.cloud

**Audience:** Developers who set `PZ_TUNNEL` to a `*.tunnel.portzero.cloud` domain, run the Port Zero daemon, and compare the Cloud tunnels section in the local management page against the dashboard at https://app.portzero.cloud.

## The lists and counts do not match

You start a service with `PZ_TUNNEL`, the local status page shows one thing under "Cloud tunnels", and the web dashboard shows different numbers or different status labels.

This is expected. The two pages read from different sources and track different things.

## Two separate views

The local dashboard (http://portzero.local) shows the cloud domains the daemon discovered on *this machine* (read from `routes.json`) plus whether this daemon's WebSocket to the cloud edge is currently open (from `cloud_state.json`).

The web dashboard shows every route the cloud has accepted for your account, no matter which machine registered it. It displays two counts:

- Active Cloud Tunnels = total rows that exist in the database right now.
- Online Now = the subset whose `last_seen_at` is less than 120 seconds old.

It also labels individual routes Online, Idle, or Offline using the same freshness value.

## First steps: observe both sides

1. Log in so cloud routes can be registered:

   ```bash
   portzero login
   ```

2. Start the daemon (it opens the local dashboard in your browser):

   ```bash
   portzero start
   ```

3. In another terminal, run a service with a cloud domain (use your real username):

   ```bash
   PZ_TUNNEL=debug-$(whoami).tunnel.portzero.cloud python -m http.server 0
   ```

4. Open http://portzero.local (or the exact URL the daemon printed).

5. In a second tab, go to https://app.portzero.cloud and look at the two stat cards and the table on the home page.

You will usually see the domain appear in both places within a few seconds, but the counts and labels can differ.

## What happens step by step

1. The daemon scans processes and containers for the `PZ_TUNNEL` environment variable.
2. It sees a name ending in `.tunnel.portzero.cloud`, classifies it as a cloud route (not a `.portzero.local` overlay route), and writes an entry to `~/.portzero/daemon/routes.json`.
3. The daemon connects (or re-uses) its WebSocket to the cloud edge and sends a `RegisterRoute` message.
4. The edge forwards the registration to the control plane API. The API validates namespace and plan limits, then upserts a row in the database and sets `last_seen_at` to the current time.
5. The local UI reads the local JSON files and shows the route under "Cloud tunnels" with the overall connection state.
6. The web dashboard fetches your routes from the API and includes the new row in "Active Cloud Tunnels".
7. If a request actually arrives at the tunnel within the next two minutes, "Online Now" also increments.

`last_seen_at` is refreshed only on registration and when the edge flushes traffic statistics (every ~30 s when there is traffic, or on disconnect). Keep-alive pings do not update it.

## Idle tunnels and "Offline" labels

A running tunnel whose WebSocket is still open can age out of "Online Now":

- < 2 minutes since last_seen → Online (green)
- < 10 minutes → Idle (yellow)
- ≥ 10 minutes with no traffic → labeled Offline (red) on the web dashboard

The row stays in the database (so it still counts as "Active") until the connection actually closes. This is why you can see an "Offline" label for a tunnel that is still up.

## Removal (no offline tunnels are kept)

Routes are deleted, not marked offline:

- Local daemon: when its scan no longer sees the original process PID (after a short grace period), it removes the entry from `routes.json` and sends an unregister message.
- Cloud: when the edge detects that the WebSocket session has closed, it deletes the row via the API.

There is no "offline tunnel" record by design. If you see a stale row on the web dashboard, either the tunnel is idle but the connection is still live, or a previous disconnect did not complete cleanup.

## Common situations

- **Multiple machines.** The web dashboard shows tunnels from every machine you have logged in from. The local UI shows only what this daemon discovered.
- **Registration rejected.** The route may still appear in the local list while the cloud shows an error or plan message in the header. The web dashboard will not list it.
- **Long-idle tunnel.** The process is alive, the local UI shows it, the web shows it under Active but with an Idle or Offline label.
- **Hard kill.** Killing the daemon or the target process without clean shutdown can leave a row on the cloud side until the next reconnect or manual cleanup.

## What to check when they disagree

- On the local page, look at the line under "Cloud tunnels" for a plan message or error.
- On the web dashboard, compare "Active Cloud Tunnels" (total rows) with "Online Now" (fresh ones). The difference is normal for idle tunnels.
- Confirm the exact domain spelling and username scope on both sides.
- Make sure `PZ_TUNNEL` was set in the environment before the target process started.

## Misunderstandings to avoid

- "Active" on the web dashboard does not mean the tunnel is currently receiving traffic. It means a database row exists.
- The local cloud tunnels list is not the list of things the internet can reach right now.
- last_seen is not a "last connected" timestamp. It is a "last registered or last saw HTTP traffic" timestamp.
- Local "connected" means this daemon's WebSocket to the edge is up. It does not mean every route in the local list has been accepted by the cloud.
- The suffix decides the path: `.tunnel.portzero.cloud` goes through the cloud edge and appears in both places; `.portzero.local` stays local only.

## Files

- `~/.portzero/daemon/routes.json` — cloud (and other) routes the local daemon knows about.
- `~/.portzero/daemon/cloud_state.json` — overall connection state, plan, and last message from the edge.
- `~/.portzero/daemon/overlay.json` — local `.portzero.local` services (separate from cloud routes).

## Commands

```bash
portzero login
portzero start
portzero start --foreground
portzero status
```

On the web dashboard the two cards appear on the home page. A fuller table is on the Cloud Tunnels page.