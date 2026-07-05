class Portzero < Formula
  desc "Eliminate port conflicts in local dev environments with virtual NIC port forwarding"
  homepage "https://portzero.cloud"
  version "0.1.0"
  license "GPL-3.0-or-later"

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
      To complete setup, run:

        sudo portzero setup

      This command documents each action before it runs, then:
        - installs the CA certificate to your system keychain so browsers trust
          *.portzero.local HTTPS
        - installs and starts the root LaunchDaemon
        - installs the scoped resolver (/etc/resolver/portzero.local) so *.portzero.local
          names resolve to the PortZero DNS server
        - pins portzero.local in /etc/hosts so your browser can reach the dashboard

      macOS mDNSResponder intercepts all *.local names before the PortZero resolver
      is consulted, so subdomains need the /etc/resolver entry and the management
      dashboard needs the static hosts entry.

      Once done, open http://portzero.local in your browser.

      Manual equivalents:
        sudo HOME="$HOME" portzero trust install
        sudo portzero autostart enable
        sudo mkdir -p /etc/resolver
        printf 'nameserver 127.0.0.1\nport 10053\n' | sudo tee /etc/resolver/portzero.local
        echo '10.254.0.2 portzero.local # portzero-local' | sudo tee -a /etc/hosts

      The running daemon also re-creates /etc/resolver/portzero.local automatically
      if it is ever removed, and notifies you when that happens. It periodically
      re-verifies the other setup steps too (CA trust, the LaunchDaemon, and the
      portzero.local hosts pin); if one regresses out-of-band it alerts you with a
      desktop notification pointing at `sudo portzero setup`.

      To stop/remove autostart: sudo portzero autostart disable
      Do not use `brew services`; it cannot pin HOME correctly for a root daemon.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/portzero --version")
  end
end
