class Portzero < Formula
  desc "Eliminate port conflicts in local dev environments with virtual NIC port forwarding"
  homepage "https://portzero.cloud"
  version "0.1.0"
  license "PolyForm-Shield-1.0.0"

  on_macos do
    on_arm do
      url "https://github.com/PortZeroNetwork/portzero-local/releases/download/v#{version}/portzero-darwin-arm64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # arm64
    end
    on_intel do
      url "https://github.com/PortZeroNetwork/portzero-local/releases/download/v#{version}/portzero-darwin-amd64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # x86_64
    end
  end

  def install
    bin.install "portzero"
  end

  def post_install
    # Generate the local CA certificate (writes to ~/Library/Application Support/PortZero/).
    # Idempotent — existing certs are kept. Does not require elevated privileges.
    system "#{bin}/portzero", "trust", "generate"
  end

  def caveats
    <<~EOS
      To complete setup, run these commands:

      1. Install the CA certificate to your system keychain so browsers trust
         *.portzero.local HTTPS (requires administrator privileges):

           sudo HOME="$HOME" portzero trust install

      2. Install and start the root LaunchDaemon (requires administrator privileges):

           sudo portzero autostart enable

         The daemon starts immediately and restarts automatically at boot.
         Plist installed at: /Library/LaunchDaemons/cloud.portzero.daemon.plist

         Note: use `sudo portzero autostart enable/disable` to manage the daemon,
         not `brew services` — the latter cannot pin HOME correctly for a root daemon.

      3. Pin portzero.local in /etc/hosts so your browser can reach the dashboard.
         macOS mDNSResponder intercepts all *.local names before the PortZero resolver
         is consulted, so the management dashboard needs a static hosts entry:

           echo '10.254.0.2 portzero.local # portzero-local' | sudo tee -a /etc/hosts

      Once done, open http://portzero.local in your browser.

      To stop/remove: sudo portzero autostart disable
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/portzero --version")
  end
end
