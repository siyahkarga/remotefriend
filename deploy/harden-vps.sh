#!/usr/bin/env bash
# Basic VPS hardening for a RemoteFriend server (Ubuntu/Debian). Safe to run again.
#   curl -sL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/harden-vps.sh | sudo bash
#
# What it does:
#   1. Automatic security updates (unattended-upgrades)
#   2. fail2ban: bans IPs that keep failing SSH logins
#   3. SSH: key-only login (password login off) -- ONLY if an SSH key is already installed
#      for root or the user who ran sudo, so you cannot lock yourself out.
# It does not enable or change the firewall (other sites on this VPS may need their ports).
set -euo pipefail

if [ "$(id -u)" -ne 0 ]; then
  echo "Run as root: use sudo." >&2
  exit 1
fi
export DEBIAN_FRONTEND=noninteractive

echo "=== 1/3 automatic security updates ==="
apt-get update -qq
apt-get install -y -qq unattended-upgrades apt-listchanges > /dev/null
cat > /etc/apt/apt.conf.d/20auto-upgrades <<'EOF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
APT::Periodic::AutocleanInterval "7";
EOF
systemctl enable --now unattended-upgrades >/dev/null 2>&1 || true
echo "Security updates will be installed automatically every day."

echo "=== 2/3 fail2ban (SSH brute-force protection) ==="
apt-get install -y -qq fail2ban > /dev/null
cat > /etc/fail2ban/jail.d/remotefriend-sshd.local <<'EOF'
[sshd]
enabled = true
maxretry = 5
findtime = 10m
bantime = 1h
EOF
systemctl enable fail2ban >/dev/null 2>&1 || true
systemctl restart fail2ban
echo "IPs with 5 failed SSH logins in 10 minutes are banned for 1 hour."

echo "=== 3/3 SSH key-only login ==="
has_key() {
  local f="$1"
  [ -s "$f" ] && grep -qE '^(ssh-(ed25519|rsa)|ecdsa-sha2-|sk-)' "$f"
}
KEY_FOUND=0
has_key /root/.ssh/authorized_keys && KEY_FOUND=1
if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != "root" ]; then
  USER_HOME="$(getent passwd "$SUDO_USER" | cut -d: -f6)"
  has_key "$USER_HOME/.ssh/authorized_keys" && KEY_FOUND=1
fi

if [ "$KEY_FOUND" = 1 ]; then
  install -d -m 0755 /etc/ssh/sshd_config.d
  cat > /etc/ssh/sshd_config.d/10-remotefriend-hardening.conf <<'EOF'
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin prohibit-password
EOF
  if sshd -t; then
    systemctl reload ssh 2>/dev/null || systemctl reload sshd 2>/dev/null || true
    echo "Password login is now OFF; log in with your SSH key."
    echo "Keep this session open and test a NEW SSH login before closing it."
  else
    rm -f /etc/ssh/sshd_config.d/10-remotefriend-hardening.conf
    echo "SSH config test failed; nothing changed."
  fi
else
  cat <<'EOF'
No SSH key is installed yet, so password login was left ON (to avoid locking you out).
To switch to key-only login:
  1. On YOUR computer (not the server) run:   ssh-keygen -t ed25519      (press Enter to accept defaults)
  2. Then:                                    ssh-copy-id root@YOUR_SERVER_IP
  3. Check that "ssh root@YOUR_SERVER_IP" logs in without a password.
  4. Run this script again.
EOF
fi

echo
echo "Done."
