import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  downloadExamples,
  runExample,
  stopExample,
  openExternal,
} from "../api";
import type {
  Status,
  Example,
  ExampleLog,
  ExampleEnd,
} from "../types";

interface Props {
  status: Status;
  onError: (message: string) => void;
  onChanged: () => void;
}

interface TermLine {
  text: string;
  kind: "out" | "cmd" | "error" | "end";
}

function stripAnsi(s: string): string {
  // eslint-disable-next-line no-control-regex
  return String(s)
    .replace(/\x1b\[[0-9;?=]*[A-Za-z]/g, "")
    .replace(/\x1b[=>]/g, "")
    .replace(/\r/g, "");
}

function osId(status: Status): string {
  return status.environment?.os?.id ?? "linux";
}
function dockerInstalled(status: Status): boolean {
  return !!status.environment?.docker?.installed;
}
function langInstalled(status: Status, langId: string): boolean {
  const items = status.languages?.items ?? [];
  return !!items.find((x) => x.id === langId)?.installed;
}
function exampleCommand(ex: Example, os: string): string {
  const c = ex.commands ?? {};
  return c[os] || c.linux || c.macos || c.windows || "";
}
function exampleUrl(ex: Example): string {
  return (
    "http://" +
    String(ex.domain ?? "")
      .replace(/:80$/, "")
      .replace(/:443$/, "") +
    "/"
  );
}
function exampleRunnable(status: Status, ex: Example): boolean {
  if (ex.requires_docker && !dockerInstalled(status)) return false;
  return langInstalled(status, ex.language);
}
function sortedExamples(status: Status): Example[] {
  const all = (status.getting_started?.examples ?? []).slice();
  const guess = status.languages?.default;
  const rank = (ex: Example) =>
    (exampleRunnable(status, ex) ? 0 : 1) * 10 +
    (ex.language === guess ? 0 : 1);
  all.sort((a, b) => {
    const r = rank(a) - rank(b);
    if (r !== 0) return r;
    return (a.title ?? a.id).localeCompare(b.title ?? b.id);
  });
  return all;
}

export default function Examples({ status, onError, onChanged }: Props) {
  const examples = status.examples;
  const downloaded = !!examples?.downloaded;
  const running = examples?.running ?? [];

  const [downloading, setDownloading] = useState(false);
  const [downloadMsg, setDownloadMsg] = useState<string>("");
  const [activeId, setActiveId] = useState<string | null>(null);
  const [activeTitle, setActiveTitle] = useState<string>("");
  const [lines, setLines] = useState<TermLine[]>([]);
  const termRef = useRef<HTMLPreElement>(null);
  const activeIdRef = useRef<string | null>(null);
  activeIdRef.current = activeId;

  // Subscribe once to the streamed example output.
  useEffect(() => {
    const unlisteners: Array<() => void> = [];
    let mounted = true;
    listen<ExampleLog>("example://log", (e) => {
      if (e.payload.id !== activeIdRef.current) return;
      setLines((prev) =>
        prev.concat({
          text: stripAnsi(e.payload.line),
          kind: e.payload.line.startsWith("$") ? "cmd" : e.payload.kind,
        }),
      );
    }).then((u) => (mounted ? unlisteners.push(u) : u()));
    listen<ExampleEnd>("example://end", (e) => {
      if (e.payload.id !== activeIdRef.current) return;
      if (e.payload.message) {
        setLines((prev) => prev.concat({ text: e.payload.message, kind: "end" }));
      }
      setActiveId(null);
      onChanged();
    }).then((u) => (mounted ? unlisteners.push(u) : u()));
    return () => {
      mounted = false;
      unlisteners.forEach((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Keep the console scrolled to the bottom as lines arrive.
  useEffect(() => {
    const el = termRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines]);

  async function doDownload() {
    setDownloading(true);
    setDownloadMsg("Downloading…");
    try {
      const res = await downloadExamples();
      setDownloadMsg(res.message ?? "Done.");
    } catch (e) {
      setDownloadMsg("");
      onError(String(e));
    } finally {
      setDownloading(false);
      onChanged();
    }
  }

  async function run(ex: Example) {
    setActiveId(ex.id);
    setActiveTitle(ex.title ?? ex.id);
    setLines([]);
    try {
      await runExample(ex.id);
    } catch (e) {
      setActiveId(null);
      onError(String(e));
    }
    onChanged();
  }

  async function stop(id: string) {
    setLines((prev) => prev.concat({ text: "[stopping…]", kind: "end" }));
    try {
      await stopExample(id);
    } catch (e) {
      onError(String(e));
    }
  }

  const os = osId(status);
  const list = sortedExamples(status);

  return (
    <section className="section">
      <div className="section-head">
        <h2>Getting started</h2>
        <p className="section-note">
          Download the examples, then run one. Each sets{" "}
          <code>PZ_TUNNEL</code> and starts a small web app.
        </p>
      </div>

      <div className="download-bar">
        <button className="button" disabled={downloading} onClick={doDownload}>
          {downloaded ? "Re-download / update" : "Download examples"}
        </button>
        {downloading && <span className="spin" />}
        <span
          className={"download-state" + (downloaded && !downloading ? " done" : "")}
        >
          {downloadMsg ||
            (downloaded
              ? `Downloaded to ${examples?.dir ?? "~/portzero-examples"}`
              : "")}
        </span>
      </div>

      {activeId !== null && (
        <div className="console">
          <div className="console-head">
            <span className="console-title">
              <span className="spin" /> {activeTitle}
            </span>
            <button className="button stop" onClick={() => stop(activeId)}>
              Stop
            </button>
          </div>
          <pre className="term" ref={termRef} aria-live="polite">
            {lines.map((l, i) => (
              <span
                key={i}
                className={
                  l.kind === "cmd"
                    ? "cmd"
                    : l.kind === "error"
                      ? "err"
                      : l.kind === "end"
                        ? "end"
                        : undefined
                }
              >
                {l.text + "\n"}
              </span>
            ))}
          </pre>
        </div>
      )}

      {list.length > 0 && (
        <div className="examples">
          {list.map((ex) => {
            const runnable = exampleRunnable(status, ex);
            const isRunning = running.includes(ex.id);
            const cmd = exampleCommand(ex, os);
            return (
              <article
                key={ex.id}
                className={"example" + (runnable ? "" : " dim")}
              >
                <div className="example-head">
                  <span className="tag lang">
                    {ex.language_label ?? ex.language}
                  </span>
                  <span className="tag">{ex.variant_label ?? ex.variant}</span>
                </div>
                <h3>{ex.title ?? ex.id}</h3>
                <pre className="code-row">
                  <code>{`$ cd ${ex.path}\n$ ${cmd}`}</code>
                </pre>
                <div className="example-actions">
                  {!runnable ? (
                    <span className="hint">
                      Install {ex.language_label ?? ex.language}
                      {ex.requires_docker && !dockerInstalled(status)
                        ? " and Docker"
                        : ""}{" "}
                      to run this.
                    </span>
                  ) : !downloaded ? (
                    <>
                      <button className="button" disabled>
                        Run
                      </button>
                      <span className="hint">Download the examples first</span>
                    </>
                  ) : isRunning ? (
                    <>
                      <button
                        className="button stop"
                        onClick={() => stop(ex.id)}
                      >
                        Stop
                      </button>
                      <button
                        className="button ghost"
                        onClick={() =>
                          openExternal(exampleUrl(ex)).catch((e) =>
                            onError(String(e)),
                          )
                        }
                      >
                        Open
                      </button>
                    </>
                  ) : (
                    <button
                      className="button"
                      disabled={activeId !== null}
                      onClick={() => run(ex)}
                    >
                      Run
                    </button>
                  )}
                </div>
              </article>
            );
          })}
        </div>
      )}
    </section>
  );
}
