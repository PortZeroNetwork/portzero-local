// Package devenvtunnel provides a thin helper for devenv-tunnel integration.
//
// The universal mechanism (no real SDK needed):
//  1. Set DEVENV_TUNNEL to a full domain name BEFORE starting your process.
//     The suffix decides the route:
//       - myapp-{branch}.devenv.local        → local virtual overlay
//       - myapp-{branch}.tunnel.devenv.tools → cloud tunnel
//     See sdks/direnv/README.md for the recommended direnv setup.
//  2. Bind your server to port 0 — the OS assigns an ephemeral port.
//  3. The devenv-tunnel daemon discovers the process, reads DEVENV_TUNNEL,
//     finds the real port, and routes traffic to it.
//
// IMPORTANT — environment visibility:
// The daemon reads each process's environment from OUTSIDE the process:
//   - Linux: /proc/<pid>/environ
//   - macOS: sysctl KERN_PROCARGS2
//
// Both sources are frozen snapshots from execve() time. Calling os.Setenv
// at runtime updates only the in-process copy and is NEVER visible to the
// daemon. This package does NOT set DEVENV_TUNNEL — set it before launch.
//
// This package has no external dependencies.
package devenvtunnel

import (
	"fmt"
	"log"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// Options controls how FindFreeListener and FindFreePort behave.
type Options struct {
	// Host is the interface to bind on. Defaults to "0.0.0.0".
	Host string

	// ServiceName is used in log output. Defaults to "app".
	ServiceName string

	// Logger is used for output. Defaults to log.Printf when nil.
	Logger func(format string, args ...any)
}

func (o *Options) host() string {
	if o.Host == "" {
		return "0.0.0.0"
	}
	return o.Host
}

func (o *Options) serviceName() string {
	if o.ServiceName == "" {
		return "app"
	}
	return o.ServiceName
}

func (o *Options) logf(format string, args ...any) {
	if o.Logger != nil {
		o.Logger(format, args...)
	} else {
		log.Printf(format, args...)
	}
}

// resolveTemplateForDisplay attempts to substitute {branch} and {worktree}
// placeholders using local git context. This is for display/logging purposes
// only — the daemon resolves templates independently from cwd/git context.
// Failure is non-fatal; unresolved placeholders remain as-is.
func resolveTemplateForDisplay(template string) (resolved string, changed bool) {
	if !strings.Contains(template, "{branch}") && !strings.Contains(template, "{worktree}") {
		return template, false
	}

	resolved = template

	if branch, err := gitOutput("rev-parse", "--abbrev-ref", "HEAD"); err == nil && branch != "" {
		resolved = strings.ReplaceAll(resolved, "{branch}", branch)
	}

	if root, err := gitOutput("rev-parse", "--show-toplevel"); err == nil && root != "" {
		resolved = strings.ReplaceAll(resolved, "{worktree}", filepath.Base(root))
	}

	return resolved, resolved != template
}

func gitOutput(args ...string) (string, error) {
	cmd := exec.Command("git", args...)
	cmd.Stderr = nil
	out, err := cmd.Output()
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(string(out)), nil
}

// ReadTunnel returns the value of DEVENV_TUNNEL from the environment, or an
// empty string if unset. This function is read-only — it does not set the
// variable. The daemon reads /proc/<pid>/environ at launch time and cannot
// see runtime os.Setenv changes.
func ReadTunnel() string {
	return os.Getenv("DEVENV_TUNNEL")
}

// FindFreeListener binds a TCP listener to port 0 and returns it along with
// the assigned port number. The listener stays open — pass it to your HTTP
// server (e.g. http.Serve(ln, handler)) or close it yourself.
//
// DEVENV_TUNNEL must already be set before the process was started.
// If unset, a WARNING is logged with setup instructions.
func FindFreeListener(opts Options) (net.Listener, int, error) {
	tunnel := ReadTunnel()

	addr := fmt.Sprintf("%s:0", opts.host())
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return nil, 0, fmt.Errorf("devenv-tunnel: failed to listen on %s: %w", addr, err)
	}

	tcpAddr, ok := ln.Addr().(*net.TCPAddr)
	if !ok {
		ln.Close() //nolint:errcheck
		return nil, 0, fmt.Errorf("devenv-tunnel: unexpected address type %T", ln.Addr())
	}
	port := tcpAddr.Port

	if tunnel != "" {
		display, changed := resolveTemplateForDisplay(tunnel)
		note := ""
		if changed {
			note = " (informational local resolution — daemon resolves independently)"
		}
		opts.logf("[devenv-tunnel] %s bound to %s:%d — tunnel domain: %s%s",
			opts.serviceName(), opts.host(), port, display, note)
	} else {
		opts.logf("[devenv-tunnel] WARNING: %s bound to %s:%d — DEVENV_TUNNEL is not set.\n"+
			"  The daemon reads the process environment at launch time (execve snapshot) and\n"+
			"  cannot see runtime os.Setenv changes.\n"+
			"  Set DEVENV_TUNNEL before starting your process:\n"+
			"    direnv:  export DEVENV_TUNNEL=myapp-$(git rev-parse --abbrev-ref HEAD).devenv.local\n"+
			"    shell:   export DEVENV_TUNNEL=myapp-{branch}.devenv.local\n"+
			"    docker:  docker run -e DEVENV_TUNNEL=myapp-{branch}.devenv.local ...\n"+
			"  See sdks/direnv/README.md for the recommended setup.",
			opts.serviceName(), opts.host(), port)
	}

	return ln, port, nil
}

// FindFreePort binds to port 0, records the port number, closes the listener,
// and returns the port. Use only when your framework requires a port integer
// rather than a net.Listener. There is a brief TOCTOU window — prefer
// FindFreeListener when possible.
func FindFreePort(opts Options) (int, error) {
	ln, port, err := FindFreeListener(opts)
	if err != nil {
		return 0, err
	}
	ln.Close() //nolint:errcheck
	return port, nil
}
