#!/bin/bash
# install-host-agent.sh - Install ZeroClawed Host-Agent on a target system
# Usage: ./install-host-agent.sh [options]
# 
# Options:
#   -b, --bind ADDR       Bind address (default: 0.0.0.0:18443)
#   -c, --config PATH     Config file path (default: /etc/clash/host-agent.toml)
#   -d, --debug           Enable debug logging
#   -h, --help            Show this help message

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIND_ADDR="0.0.0.0:18443"
CONFIG_PATH="/etc/clash/host-agent.toml"
DEBUG=false

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -b|--bind)
            BIND_ADDR="$2"
            shift 2
            ;;
        -c|--config)
            CONFIG_PATH="$2"
            shift 2
            ;;
        -d|--debug)
            DEBUG=true
            shift
            ;;
        -h|--help)
            echo "ZeroClawed Host-Agent Installer"
            echo ""
            echo "Usage: $0 [options]"
            echo ""
            echo "Options:"
            echo "  -b, --bind ADDR       Bind address (default: 0.0.0.0:18443)"
            echo "  -c, --config PATH     Config file path (default: /etc/clash/host-agent.toml)"
            echo "  -d, --debug           Enable debug logging"
            echo "  -h, --help            Show this help message"
            exit 0
            ;;
        *)
            log_error "Unknown option: $1"
            exit 1
            ;;
    esac
done

# Check prerequisites
check_prerequisites() {
    log_info "Checking prerequisites..."
    
    # Check for root
    if [[ $EUID -ne 0 ]]; then
        log_error "This script must be run as root"
        exit 1
    fi
    
    # Check for required commands
    for cmd in openssl systemctl; do
        if ! command -v "$cmd" &> /dev/null; then
            log_error "Required command not found: $cmd"
            exit 1
        fi
    done
    
    # Check for ZFS
    if ! command -v zfs &> /dev/null; then
        log_warn "ZFS not found. ZFS operations will not work."
    fi
    
    # Check for clash-host-agent binary
    if [[ ! -f "./clash-host-agent" ]]; then
        log_error "clash-host-agent binary not found in current directory"
        log_info "Please build the binary first: cargo build --release -p host-agent"
        exit 1
    fi
    
    log_info "Prerequisites check passed"
}

# Create user and directories
setup_user() {
    log_info "Setting up clash-agent user..."
    
    if ! id -u clash-agent &>/dev/null; then
        useradd -r -s /bin/false -d /var/lib/clash -m clash-agent
        log_info "Created clash-agent user"
    else
        log_info "clash-agent user already exists"
    fi
    
    # Create directories
    mkdir -p /etc/clash/certs /var/log/clash /var/lib/clash
    chown -R clash-agent:clash-agent /etc/clash /var/log/clash /var/lib/clash
    chmod 750 /etc/clash /var/log/clash
}

# Generate certificates
generate_certs() {
    log_info "Generating mTLS certificates..."
    
    local CERT_DIR="/etc/clash/certs"
    
    if [[ -f "$CERT_DIR/server.crt" ]]; then
        log_warn "Certificates already exist. Skipping generation."
        log_info "Delete $CERT_DIR to regenerate certificates."
        return
    fi
    
    cd "$CERT_DIR"
    
    # Generate CA
    openssl genrsa -out ca.key 4096 2>/dev/null
    openssl req -new -x509 -key ca.key -sha256 \
        -subj '/C=US/O=ZeroClawed/CN=ZeroClawed CA' \
        -days 3650 -out ca.crt 2>/dev/null
    
    # Generate server cert
    openssl genrsa -out server.key 4096 2>/dev/null
    openssl req -subj '/CN=host-agent' -sha256 \
        -key server.key -out server.csr 2>/dev/null
    openssl x509 -req -in server.csr \
        -CA ca.crt -CAkey ca.key -CAcreateserial \
        -out server.crt -days 365 -sha256 2>/dev/null
    
    # Generate client cert for librarian
    openssl genrsa -out librarian.key 4096 2>/dev/null
    openssl req -subj '/CN=librarian' -sha256 \
        -key librarian.key -out librarian.csr 2>/dev/null
    openssl x509 -req -in librarian.csr \
        -CA ca.crt -CAkey ca.key -CAcreateserial \
        -out librarian.crt -days 365 -sha256 2>/dev/null
    cat librarian.crt librarian.key > librarian-bundle.pem
    
    # Set permissions
    chmod 600 *.key
    chmod 644 *.crt
    chown -R clash-agent:clash-agent .
    
    log_info "Certificates generated in $CERT_DIR"
}

# Install binary
install_binary() {
    log_info "Installing clash-host-agent binary..."
    
    cp "./clash-host-agent" /usr/local/bin/
    chmod 755 /usr/local/bin/clash-host-agent
    
    log_info "Binary installed to /usr/local/bin/clash-host-agent"
}

# Create configuration
create_config() {
    log_info "Creating configuration file..."
    
    cat > "$CONFIG_PATH" << EOF
[server]
bind = "$BIND_ADDR"
cert = "/etc/clash/certs/server.crt"
key = "/etc/clash/certs/server.key"
client_ca = "/etc/clash/certs/ca.crt"

[audit]
log_path = "/var/log/clash/audit.jsonl"
rotation = "daily"
retention_days = 90

[approval]
enabled = true
ttl_seconds = 300
token_entropy_bits = 80
allowed_approvers = []

[metrics]
enabled = true
bind = "127.0.0.1:19090"

[[agent]]
cn_pattern = "librarian*"
agent_type = "librarian"
unix_user = "root"
autonomy = "supervised"
allowed_operations = ["zfs-list", "zfs-snapshot"]
requires_approval_for = ["zfs-destroy"]

[[rule]]
operation = "zfs-destroy"
approval_required = true

[[rule]]
operation = "zfs-snapshot"
approval_required = false

[[rule]]
operation = "zfs-list"
approval_required = false
EOF
    
    chown clash-agent:clash-agent "$CONFIG_PATH"
    chmod 640 "$CONFIG_PATH"
    
    log_info "Configuration created at $CONFIG_PATH"
}

# Install OS-level wrapper scripts (root-owned, mode 0755)
install_wrappers() {
    log_info "Installing OS wrapper scripts to /usr/local/sbin/ ..."

    local wrapper_src="$SCRIPT_DIR/wrappers"

    if [[ ! -d "$wrapper_src" ]]; then
        log_warn "Wrappers directory $wrapper_src not found; skipping wrapper install"
        return
    fi

    for wrapper in pct-create-wrapper zfs-destroy-wrapper git-safe-wrapper; do
        local src="$wrapper_src/$wrapper"
        local dst="/usr/local/sbin/$wrapper"

        if [[ ! -f "$src" ]]; then
            log_warn "Wrapper $wrapper not found at $src; skipping"
            continue
        fi

        install -o root -g root -m 0755 "$src" "$dst"
        log_info "  Installed $dst (root:root 0755)"
    done

    log_info "Wrapper scripts installed"
}

# Install logrotate configuration
install_logrotate() {
    log_info "Installing logrotate configuration..."

    local src="$SCRIPT_DIR/wrappers/logrotate-clash-host-agent"
    local dst="/etc/logrotate.d/clash-host-agent"

    if [[ ! -f "$src" ]]; then
        log_warn "Logrotate template not found at $src; skipping"
        return
    fi

    install -o root -g root -m 0644 "$src" "$dst"
    log_info "Logrotate configuration installed at $dst"

    # Validate it
    if command -v logrotate &>/dev/null; then
        if logrotate --debug "$dst" > /dev/null 2>&1; then
            log_info "logrotate syntax check passed"
        else
            log_warn "logrotate debug run returned warnings — review $dst"
        fi
    fi
}

# Install sudoers fragment (wrapper-only policy — replaces broad /sbin/zfs entry)
install_sudoers() {
    log_info "Setting up sudoers (wrapper-only policy)..."

    local template="$SCRIPT_DIR/wrappers/sudoers-clash-agent.template"
    local dst="/etc/sudoers.d/clash-agent"
    local agent_user="${CLASH_AGENT_USER:-clash-agent}"

    if [[ ! -f "$template" ]]; then
        log_warn "Sudoers template not found at $template; writing minimal fallback"
        cat > "$dst" << SUDOERS_EOF
# ZeroClawed Host-Agent sudoers — minimal fallback
${agent_user} ALL=(root) NOPASSWD: /usr/local/sbin/pct-create-wrapper
${agent_user} ALL=(root) NOPASSWD: /usr/local/sbin/zfs-destroy-wrapper
${agent_user} ALL=(root) NOPASSWD: /usr/local/sbin/git-safe-wrapper
${agent_user} ALL=(root) NOPASSWD: /usr/sbin/pct status *
${agent_user} ALL=(root) NOPASSWD: /sbin/zfs list *
${agent_user} ALL=(root) NOPASSWD: /sbin/zfs get *
${agent_user} ALL=(root) NOPASSWD: /sbin/zfs snapshot *
SUDOERS_EOF
    else
        # Substitute {{CLASH_AGENT_USER}} placeholder
        sed "s/{{CLASH_AGENT_USER}}/${agent_user}/g" "$template" > "$dst"
    fi

    chmod 440 "$dst"

    # Validate
    if ! visudo -cf "$dst"; then
        log_error "Sudoers file validation failed — removing $dst"
        rm -f "$dst"
        exit 1
    fi

    log_info "Sudoers configured at $dst"
}

# setup_sudoers is now replaced by install_sudoers (kept as alias for backwards compat)
setup_sudoers() {
    install_sudoers
}

# Create systemd service
create_service() {
    log_info "Creating systemd service..."
    
    cat > /etc/systemd/system/clash-host-agent.service << 'EOF'
[Unit]
Description=ZeroClawed Host-Agent mTLS RPC Server
After=network.target

[Service]
Type=notify
ExecStart=/usr/local/bin/clash-host-agent --config /etc/clash/host-agent.toml
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=5
User=clash-agent
Group=clash-agent
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/log/clash
AmbientCapabilities=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
EOF
    
    systemctl daemon-reload
    log_info "Systemd service created"
}

# Start service
start_service() {
    log_info "Starting clash-host-agent service..."
    
    systemctl enable clash-host-agent
    systemctl start clash-host-agent
    
    sleep 2
    
    if systemctl is-active --quiet clash-host-agent; then
        log_info "Service started successfully"
    else
        log_error "Service failed to start"
        systemctl status clash-host-agent --no-pager
        exit 1
    fi
}

# Test installation
test_installation() {
    log_info "Testing installation..."
    
    # Test health endpoint
    if curl -sfk --cert /etc/clash/certs/librarian-bundle.pem \
        https://localhost:18443/health > /dev/null 2>&1; then
        log_info "Health check passed"
    else
        log_warn "Health check failed - service may still be starting"
    fi
    
    # Test ZFS list
    if curl -sfk --cert /etc/clash/certs/librarian-bundle.pem \
        -X POST -H "Content-Type: application/json" \
        -d '{"dataset": "tank"}' \
        https://localhost:18443/zfs/list > /dev/null 2>&1; then
        log_info "ZFS list test passed"
    else
        log_warn "ZFS list test failed - may need ZFS setup"
    fi
}

# Main installation
main() {
    log_info "ZeroClawed Host-Agent Installer"
    log_info "=============================="
    
    check_prerequisites
    setup_user
    generate_certs
    install_binary
    install_wrappers
    create_config
    install_sudoers
    install_logrotate
    create_service
    start_service
    test_installation
    
    log_info "=============================="
    log_info "Installation complete!"
    log_info ""
    log_info "Service status:"
    systemctl status clash-host-agent --no-pager -l
    log_info ""
    log_info "Client certificate for librarian:"
    log_info "  /etc/clash/certs/librarian-bundle.pem"
    log_info ""
    log_info "To fetch the client certificate to your machine:"
    log_info "  scp root@<host>:/etc/clash/certs/librarian-bundle.pem ./"
    log_info ""
    log_info "Test the service:"
    log_info "  curl -k --cert librarian-bundle.pem https://<host>:18443/health"
}

main "$@"
