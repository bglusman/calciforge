#!/bin/bash
# ZeroClawed v3 Host-Agent Installation Script
# Run on Proxmox host (10.0.0.70) as root

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
CLASH_USER="clash-agent"
CLASH_GROUP="clash"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/clash"
CERT_DIR="${CONFIG_DIR}/certs"
LOG_DIR="/var/log/clash"
SERVICE_NAME="clash-host-agent"
BINARY_NAME="clash-host-agent"

# Parse arguments
DRY_RUN=false
SKIP_BUILD=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        --skip-build)
            SKIP_BUILD=true
            shift
            ;;
        --help)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --dry-run      Show what would be done without making changes"
            echo "  --skip-build   Skip building the binary (use existing)"
            echo "  --help         Show this help message"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

dry_run_echo() {
    if [[ "$DRY_RUN" == "true" ]]; then
        echo -e "${YELLOW}[DRY-RUN]${NC} Would execute: $1"
    fi
}

# Check if running as root
check_root() {
    if [[ "$DRY_RUN" == "false" ]] && [[ $EUID -ne 0 ]]; then
        log_error "This script must be run as root"
        exit 1
    fi
}

# Create clash user and group
create_user() {
    log_info "Creating user and group..."
    
    # Create group if not exists
    if ! getent group "$CLASH_GROUP" &>/dev/null; then
        if [[ "$DRY_RUN" == "true" ]]; then
            dry_run_echo "groupadd --system $CLASH_GROUP"
        else
            groupadd --system "$CLASH_GROUP"
            log_info "Created group: $CLASH_GROUP"
        fi
    else
        log_info "Group $CLASH_GROUP already exists"
    fi
    
    # Create user if not exists
    if ! id "$CLASH_USER" &>/dev/null; then
        if [[ "$DRY_RUN" == "true" ]]; then
            dry_run_echo "useradd --system --gid $CLASH_GROUP --home-dir /var/lib/clash --shell /bin/false $CLASH_USER"
        else
            useradd --system --gid "$CLASH_GROUP" --home-dir /var/lib/clash --shell /bin/false "$CLASH_USER"
            log_info "Created user: $CLASH_USER"
        fi
    else
        log_info "User $CLASH_USER already exists"
    fi
}

# Create directories
create_directories() {
    log_info "Creating directories..."
    
    local dirs=("$CONFIG_DIR" "$CERT_DIR" "$LOG_DIR")
    
    for dir in "${dirs[@]}"; do
        if [[ "$DRY_RUN" == "true" ]]; then
            dry_run_echo "mkdir -p $dir"
        else
            mkdir -p "$dir"
        fi
    done
    
    if [[ "$DRY_RUN" == "false" ]]; then
        chown root:"$CLASH_GROUP" "$LOG_DIR"
        chmod 775 "$LOG_DIR"
        log_info "Set permissions on $LOG_DIR"
    fi
}

# Build the binary
build_binary() {
    if [[ "$SKIP_BUILD" == "true" ]]; then
        log_info "Skipping build (--skip-build specified)"
        return 0
    fi
    
    log_info "Building host-agent binary..."
    
    local project_dir="/root/projects/zeroclawed"
    
    if [[ ! -d "$project_dir" ]]; then
        log_error "Project directory not found: $project_dir"
        exit 1
    fi
    
    if [[ "$DRY_RUN" == "true" ]]; then
        dry_run_echo "cd $project_dir && cargo build --release -p host-agent"
    else
        cd "$project_dir"
        cargo build --release -p host-agent
        log_info "Build complete"
    fi
}

# Install the binary
install_binary() {
    log_info "Installing binary..."
    
    local binary_path="/root/projects/zeroclawed/target/release/${BINARY_NAME}"
    
    if [[ "$DRY_RUN" == "true" ]]; then
        dry_run_echo "cp $binary_path $INSTALL_DIR/"
        dry_run_echo "chmod 755 ${INSTALL_DIR}/${BINARY_NAME}"
        dry_run_echo "chown root:root ${INSTALL_DIR}/${BINARY_NAME}"
    else
        if [[ ! -f "$binary_path" ]]; then
            log_error "Binary not found: $binary_path"
            log_error "Run without --skip-build or build first with: cargo build --release -p host-agent"
            exit 1
        fi
        
        cp "$binary_path" "$INSTALL_DIR/"
        chmod 755 "${INSTALL_DIR}/${BINARY_NAME}"
        chown root:root "${INSTALL_DIR}/${BINARY_NAME}"
        log_info "Installed binary to ${INSTALL_DIR}/${BINARY_NAME}"
    fi
}

# Generate certificates
generate_certs() {
    log_info "Generating test certificates..."
    
    if [[ "$DRY_RUN" == "true" ]]; then
        dry_run_echo "openssl genrsa -out ${CERT_DIR}/ca.key 4096"
        dry_run_echo "openssl req -new -x509 -days 3650 -key ${CERT_DIR}/ca.key -out ${CERT_DIR}/ca.crt -subj '/CN=ZeroClawed CA'"
        return 0
    fi
    
    # Check if certs already exist
    if [[ -f "${CERT_DIR}/ca.crt" ]] && [[ -f "${CERT_DIR}/server.crt" ]]; then
        log_warn "Certificates already exist. Use existing certs? (y/n)"
        read -r response
        if [[ "$response" =~ ^[Yy]$ ]]; then
            log_info "Using existing certificates"
            return 0
        fi
    fi
    
    # Generate CA
    openssl genrsa -out "${CERT_DIR}/ca.key" 4096
    openssl req -new -x509 -days 3650 -key "${CERT_DIR}/ca.key" -out "${CERT_DIR}/ca.crt" -subj "/CN=ZeroClawed CA"
    
    # Generate server cert
    openssl genrsa -out "${CERT_DIR}/server.key" 2048
    openssl req -new -key "${CERT_DIR}/server.key" -out "${CERT_DIR}/server.csr" -subj "/CN=host-agent.proxmox"
    openssl x509 -req -in "${CERT_DIR}/server.csr" -CA "${CERT_DIR}/ca.crt" -CAkey "${CERT_DIR}/ca.key" -CAcreateserial -out "${CERT_DIR}/server.crt" -days 365
    
    # Generate client cert for librarian
    openssl genrsa -out "${CERT_DIR}/client-librarian.key" 2048
    openssl req -new -key "${CERT_DIR}/client-librarian.key" -out "${CERT_DIR}/client-librarian.csr" -subj "/CN=librarian"
    openssl x509 -req -in "${CERT_DIR}/client-librarian.csr" -CA "${CERT_DIR}/ca.crt" -CAkey "${CERT_DIR}/ca.key" -CAcreateserial -out "${CERT_DIR}/client-librarian.crt" -days 365
    
    # Set permissions
    chmod 600 "${CERT_DIR}"/*.key
    chmod 644 "${CERT_DIR}"/*.crt
    chown -R root:"$CLASH_GROUP" "$CERT_DIR"
    
    log_info "Certificates generated in ${CERT_DIR}"
}

# Create configuration
create_config() {
    log_info "Creating configuration..."
    
    local config_file="${CONFIG_DIR}/host-agent.toml"
    
    if [[ -f "$config_file" ]] && [[ "$DRY_RUN" == "false" ]]; then
        log_warn "Config file already exists: $config_file"
        return 0
    fi
    
    cat > /tmp/host-agent.toml << 'EOF'
[server]
bind = "127.0.0.1:18443"
cert = "/etc/clash/certs/server.crt"
key = "/etc/clash/certs/server.key"
client_ca = "/etc/clash/certs/ca.crt"

[audit]
log_path = "/var/log/clash/audit.jsonl"

[approval]
enabled = true
ttl_seconds = 300
signal_webhook = "http://10.0.0.20:18796/webhooks/signal"

[[rules]]
operation = "zfs-destroy"
approval_required = true

[[rules]]
operation = "zfs-snapshot"
approval_required = false

[[rules]]
operation = "zfs-list"
approval_required = false
EOF

    if [[ "$DRY_RUN" == "true" ]]; then
        dry_run_echo "cp /tmp/host-agent.toml $config_file"
        dry_run_echo "chmod 644 $config_file"
    else
        cp /tmp/host-agent.toml "$config_file"
        chmod 644 "$config_file"
        log_info "Created config: $config_file"
    fi
}

# Configure ZFS delegation
configure_zfs() {
    log_info "Configuring ZFS delegation..."
    
    local datasets=("tank/media" "tank/system" "tank/backup")
    
    for dataset in "${datasets[@]}"; do
        if zfs list "$dataset" &>/dev/null; then
            if [[ "$DRY_RUN" == "true" ]]; then
                dry_run_echo "zfs allow -u $CLASH_USER snapshot,list,hold,send $dataset"
            else
                zfs allow -u "$CLASH_USER" snapshot,list,hold,send "$dataset"
                log_info "Delegated snapshot permissions on $dataset"
            fi
        else
            log_warn "Dataset does not exist: $dataset"
        fi
    done
}

# Configure sudoers
configure_sudoers() {
    log_info "Configuring sudoers..."
    
    local sudoers_file="/etc/sudoers.d/${CLASH_USER}"
    
    cat > /tmp/clash-sudoers << EOF
# ZeroClawed Host-Agent sudoers configuration
# Destructive operations require sudo
$CLASH_USER ALL=(root) NOPASSWD: /sbin/zfs destroy *
$CLASH_USER ALL=(root) NOPASSWD: /sbin/zfs rollback *
$CLASH_USER ALL=(root) NOPASSWD: /usr/bin/systemctl restart [0-9]*
$CLASH_USER ALL=(root) NOPASSWD: /usr/bin/systemctl stop [0-9]*
$CLASH_USER ALL=(root) NOPASSWD: /usr/sbin/pct start *
$CLASH_USER ALL=(root) NOPASSWD: /usr/sbin/pct stop *
$CLASH_USER ALL=(root) NOPASSWD: /usr/sbin/pct status *
EOF

    if [[ "$DRY_RUN" == "true" ]]; then
        dry_run_echo "cp /tmp/clash-sudoers $sudoers_file"
        dry_run_echo "chmod 440 $sudoers_file"
        dry_run_echo "visudo -c"
    else
        cp /tmp/clash-sudoers "$sudoers_file"
        chmod 440 "$sudoers_file"
        
        # Validate sudoers
        if visudo -c; then
            log_info "Sudoers configuration validated"
        else
            log_error "Sudoers configuration failed validation"
            rm -f "$sudoers_file"
            exit 1
        fi
    fi
}

# Setup audit log
setup_audit_log() {
    log_info "Setting up audit log..."
    
    local log_file="${LOG_DIR}/audit.jsonl"
    
    if [[ "$DRY_RUN" == "true" ]]; then
        dry_run_echo "touch $log_file"
        dry_run_echo "chown root:$CLASH_GROUP $log_file"
        dry_run_echo "chmod 664 $log_file"
        dry_run_echo "chattr +a $log_file"
    else
        touch "$log_file"
        chown root:"$CLASH_GROUP" "$log_file"
        chmod 664 "$log_file"
        
        # Try to set append-only (may fail on some filesystems)
        if chattr +a "$log_file" 2>/dev/null; then
            log_info "Set append-only attribute on audit log"
        else
            log_warn "Could not set append-only attribute (filesystem may not support it)"
        fi
    fi
}

# Create systemd service
create_systemd_service() {
    log_info "Creating systemd service..."
    
    cat > /tmp/clash-host-agent.service << 'EOF'
[Unit]
Description=ZeroClawed Host-Agent
After=network.target

[Service]
Type=simple
User=clash-agent
Group=clash
ExecStart=/usr/local/bin/clash-host-agent --config /etc/clash/host-agent.toml
Restart=always
RestartSec=5

# Hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/log/clash
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true

[Install]
WantedBy=multi-user.target
EOF

    if [[ "$DRY_RUN" == "true" ]]; then
        dry_run_echo "cp /tmp/clash-host-agent.service /etc/systemd/system/"
        dry_run_echo "systemctl daemon-reload"
    else
        cp /tmp/clash-host-agent.service /etc/systemd/system/
        systemctl daemon-reload
        log_info "Created systemd service"
    fi
}

# Enable and start service
enable_service() {
    log_info "Enabling service..."
    
    if [[ "$DRY_RUN" == "true" ]]; then
        dry_run_echo "systemctl enable clash-host-agent.service"
        return 0
    fi
    
    systemctl enable clash-host-agent.service
    log_info "Service enabled. Start with: systemctl start clash-host-agent"
}

# Print summary
print_summary() {
    echo ""
    echo "==================================="
    echo "Installation Summary"
    echo "==================================="
    
    if [[ "$DRY_RUN" == "true" ]]; then
        echo -e "${YELLOW}This was a DRY RUN. No changes were made.${NC}"
        echo "Run without --dry-run to apply changes."
    else
        echo -e "${GREEN}Installation complete!${NC}"
    fi
    
    echo ""
    echo "Next steps:"
    echo "  1. Review configuration: ${CONFIG_DIR}/host-agent.toml"
    echo "  2. Distribute client certificates from: ${CERT_DIR}/client-librarian.*"
    echo "  3. Start service: systemctl start clash-host-agent"
    echo "  4. Check status: systemctl status clash-host-agent"
    echo "  5. Test: bash scripts/test-host-agent.sh"
    echo ""
    echo "Security checklist:"
    echo "  [ ] Replace test certificates with production CA-issued certs"
    echo "  [ ] Enable append-only audit log: chattr +a ${LOG_DIR}/audit.jsonl"
    echo "  [ ] Restrict config directory: chmod 700 ${CERT_DIR}"
    echo "  [ ] Review sudoers rules regularly"
    echo "  [ ] Monitor audit log for anomalies"
}

# Main execution
main() {
    echo "ZeroClawed v3 Host-Agent Installer"
    echo "================================="
    
    if [[ "$DRY_RUN" == "true" ]]; then
        echo -e "${YELLOW}DRY RUN MODE - No changes will be made${NC}"
        echo ""
    fi
    
    check_root
    create_user
    create_directories
    build_binary
    install_binary
    generate_certs
    create_config
    configure_zfs
    configure_sudoers
    setup_audit_log
    create_systemd_service
    enable_service
    
    print_summary
}

# Run main
main "$@"
