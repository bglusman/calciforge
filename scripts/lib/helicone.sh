#!/usr/bin/env bash
# scripts/lib/helicone.sh — Helicone install and repair helpers.
#
# Ownership:
#   Local Helicone AI Gateway and optional dashboard/container bootstrap.
#
# Required globals:
#   CALCIFORGE_CONFIG_HOME
#   CALCIFORGE_GATEWAY_UI_URL
#   CALCIFORGE_HELICONE_* defaults
#   HOME, IS_ROOT, LOG_DIR, PLATFORM, PLIST_DIR, SYSTEMCTL, ZC_LOG_DIR
#
# Required functions from common.sh / install.sh:
#   die, enable_restart_service, load_launch_agent, ok, random_hex,
#   require_npm, truthy, warn.

[[ -n "${_CALCIFORGE_HELICONE_LIB_LOADED:-}" ]] && return 0
_CALCIFORGE_HELICONE_LIB_LOADED=1

calciforge_lan_ip() {
    if [[ "$PLATFORM" == "Darwin" ]]; then
        ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null || true
        return 0
    fi
    if command -v ip >/dev/null 2>&1; then
        ip route get 1.1.1.1 2>/dev/null | awk '{for (i=1; i<=NF; i++) if ($i == "src") {print $(i+1); exit}}'
        return 0
    fi
    hostname -I 2>/dev/null | awk '{print $1}'
}

helicone_install_dir() {
    if [[ "$PLATFORM" == "Darwin" ]] || [[ $EUID -ne 0 ]]; then
        printf '%s\n' "$HOME/.local/share/calciforge/helicone-ai-gateway"
    else
        printf '%s\n' "/opt/calciforge/helicone-ai-gateway"
    fi
}

helicone_ai_gateway_bin() {
    local dir="$1"
    local real_bin="$dir/node_modules/@helicone/ai-gateway/node_modules/.bin_real/ai-gateway"
    local wrapper_bin="$dir/node_modules/.bin/ai-gateway"
    [[ -x "$real_bin" ]] && printf '%s\n' "$real_bin" || printf '%s\n' "$wrapper_bin"
}

systemd_exec_arg() {
    python3 - "$1" <<'PY'
import sys

arg = sys.argv[1].replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%")
print(f'"{arg}"')
PY
}

docker_publish_bind() {
    local bind="$1"
    if [[ "$bind" == "::" ]]; then
        printf '[::]'
    elif [[ "$bind" == *:* && "$bind" != \[*\] ]]; then
        printf '[%s]' "$bind"
    else
        printf '%s' "$bind"
    fi
}

ensure_helicone_key() {
    mkdir -p "$(dirname "$CALCIFORGE_HELICONE_API_KEY_FILE")"
    if [[ -s "$CALCIFORGE_HELICONE_API_KEY_FILE" ]]; then
        chmod 600 "$CALCIFORGE_HELICONE_API_KEY_FILE" 2>/dev/null || true
        return 0
    fi
    ( umask 077; printf 'cfh_%s\n' "$(random_hex 24 'Helicone local API key')" > "$CALCIFORGE_HELICONE_API_KEY_FILE" )
    chmod 600 "$CALCIFORGE_HELICONE_API_KEY_FILE"
}

write_helicone_ai_gateway_config() {
    local config_path="$1"
    mkdir -p "$(dirname "$config_path")"
    python3 - "$config_path" "$CALCIFORGE_HELICONE_AI_GATEWAY_PORT" "$CALCIFORGE_HELICONE_JAWN_PORT" "$CALCIFORGE_HELICONE_API_KEY_FILE" "$CALCIFORGE_HELICONE_PROVIDER" "$CALCIFORGE_HELICONE_OLLAMA_BASE_URL" "$CALCIFORGE_HELICONE_MODELS" <<'PY'
import json
import pathlib
import sys

config_path, port, jawn_port, key_file, provider, ollama_url, models_csv = sys.argv[1:]
models = [m.strip() for m in models_csv.split(",") if m.strip()]
api_key = pathlib.Path(key_file).read_text().strip()

def yq(value):
    return json.dumps(str(value))

lines = [
    "server:",
    "  address: 127.0.0.1",
    f"  port: {port}",
    "",
    "helicone:",
    "  features: all",
    f"  api-key: {yq(api_key)}",
    f"  base-url: {yq(f'http://127.0.0.1:{jawn_port}')}",
    f"  websocket-url: {yq(f'ws://127.0.0.1:{jawn_port}/ws/v1/router/control-plane')}",
    "",
    "providers:",
]
if provider == "ollama" and models:
    lines.extend([
        "  ollama:",
        "    models:",
        *[f"      - {yq(model)}" for model in models],
        f"    base-url: {yq(ollama_url)}",
        "",
        "routers:",
        "  calciforge:",
        "    load-balance:",
        "      chat:",
        "        strategy: weighted",
        "        providers:",
        "          - provider: ollama",
        "            weight: 1",
    ])
else:
    lines.extend(["  {}", ""])
pathlib.Path(config_path).write_text("\n".join(lines) + "\n")
PY
    chmod 600 "$config_path" 2>/dev/null || true
}

helicone_dashboard_password_hash() {
    local password="$1"
    printf '%s' "$password" | docker exec -i "$CALCIFORGE_HELICONE_CONTAINER" node -e '
const { hashPassword } = require("/app/node_modules/better-auth/dist/crypto/index.cjs");

let password = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", chunk => { password += chunk; });
process.stdin.on("end", async () => {
  try {
    console.log(await hashPassword(password));
  } catch (error) {
    console.error(error);
    process.exit(1);
  }
});
'
}

seed_helicone_api_key() {
    truthy "$CALCIFORGE_HELICONE_DASHBOARD_ENABLED" || return 0
    command -v docker >/dev/null 2>&1 || return 0
    docker ps --format '{{.Names}}' | grep -qx "$CALCIFORGE_HELICONE_CONTAINER" || return 0

    local key hash dashboard_email dashboard_name dashboard_password dashboard_password_hash
    key="$(tr -d '\n' < "$CALCIFORGE_HELICONE_API_KEY_FILE")"
    hash="$(python3 - "$key" <<'PY'
import hashlib, sys
print(hashlib.sha256(("Bearer " + sys.argv[1]).encode()).hexdigest())
PY
)"
    dashboard_email="$CALCIFORGE_HELICONE_DASHBOARD_USER_EMAIL"
    dashboard_name="$CALCIFORGE_HELICONE_DASHBOARD_USER_NAME"
    dashboard_password=""
    dashboard_password_hash=""
    if [[ -n "$CALCIFORGE_HELICONE_DASHBOARD_PASSWORD_FILE" && -s "$CALCIFORGE_HELICONE_DASHBOARD_PASSWORD_FILE" ]]; then
        chmod 600 "$CALCIFORGE_HELICONE_DASHBOARD_PASSWORD_FILE" 2>/dev/null || \
            warn "Could not restrict Helicone dashboard password file permissions: $CALCIFORGE_HELICONE_DASHBOARD_PASSWORD_FILE"
        if find "$CALCIFORGE_HELICONE_DASHBOARD_PASSWORD_FILE" -perm -077 -print -quit 2>/dev/null | grep -q .; then
            warn "Helicone dashboard password file is group/world-readable: $CALCIFORGE_HELICONE_DASHBOARD_PASSWORD_FILE"
        fi
        dashboard_password="$(tr -d '\n' < "$CALCIFORGE_HELICONE_DASHBOARD_PASSWORD_FILE")"
    elif [[ -n "$CALCIFORGE_HELICONE_DASHBOARD_PASSWORD" ]]; then
        dashboard_password="$CALCIFORGE_HELICONE_DASHBOARD_PASSWORD"
    fi
    if [[ -n "$dashboard_email" && -n "$dashboard_password" ]]; then
        dashboard_password_hash="$(helicone_dashboard_password_hash "$dashboard_password")" || \
            warn "Could not hash Helicone dashboard password; dashboard user will be created without password repair"
    fi
    for _ in {1..30}; do
        if docker exec "$CALCIFORGE_HELICONE_CONTAINER" sh -lc \
            'PGPASSWORD="$POSTGRES_PASSWORD" psql -h 127.0.0.1 -U "${POSTGRES_USER:-postgres}" -d "${POSTGRES_DB:-helicone_test}" -Atc "select to_regclass('\''public.helicone_api_keys'\''), to_regclass('\''public.\"user\"'\''), to_regclass('\''public.account'\''), to_regclass('\''public.organization'\''), to_regclass('\''public.organization_member'\'')" 2>/dev/null | grep -q "helicone_api_keys.*user.*account.*organization.*organization_member"'; then
            break
        fi
        sleep 1
    done
    docker exec \
        -e HELICONE_LOCAL_KEY_HASH="$hash" \
        -e HELICONE_DASHBOARD_EMAIL="$dashboard_email" \
        -e HELICONE_DASHBOARD_NAME="$dashboard_name" \
        -e HELICONE_DASHBOARD_PASSWORD_HASH="$dashboard_password_hash" \
        -e HELICONE_ORG_NAME="$CALCIFORGE_HELICONE_ORG_NAME" \
        "$CALCIFORGE_HELICONE_CONTAINER" sh -lc '
PGPASSWORD="$POSTGRES_PASSWORD" psql -h 127.0.0.1 -U "${POSTGRES_USER:-postgres}" -d "${POSTGRES_DB:-helicone_test}" \
  -v ON_ERROR_STOP=1 \
  -v key_hash="$HELICONE_LOCAL_KEY_HASH" \
  -v dashboard_email="$HELICONE_DASHBOARD_EMAIL" \
  -v dashboard_name="$HELICONE_DASHBOARD_NAME" \
  -v dashboard_password_hash="$HELICONE_DASHBOARD_PASSWORD_HASH" \
  -v org_name="$HELICONE_ORG_NAME" >/dev/null <<'\''SQL'\''
insert into public."user" (id, name, email, "emailVerified", "createdAt", "updatedAt", auth_user_id)
values ('\''calciforge-local'\'', '\''Calciforge Local'\'', '\''calciforge-local@example.invalid'\'', true, now(), now(), '\''00000000-0000-4000-8000-000000000001'\'')
on conflict (id) do update set name = excluded.name, email = excluded.email, "emailVerified" = excluded."emailVerified", "updatedAt" = now(), auth_user_id = excluded.auth_user_id;

insert into public.organization (id, name, owner, is_personal, created_at)
values ('\''00000000-0000-4000-8000-000000000002'\'', coalesce(nullif(:'\''org_name'\'', '\'''\''), '\''Calciforge Local'\''), '\''00000000-0000-4000-8000-000000000001'\'', true, now())
on conflict (id) do update set name = excluded.name, owner = excluded.owner;

insert into public.organization_member (id, member, "user", organization, org_role, created_at)
values ('\''00000000-0000-4000-8000-000000000003'\'', '\''00000000-0000-4000-8000-000000000001'\'', '\''calciforge-local'\'', '\''00000000-0000-4000-8000-000000000002'\'', '\''owner'\'', now())
on conflict (id) do update set member = excluded.member, "user" = excluded."user", organization = excluded.organization, org_role = excluded.org_role;

-- Helicone has used both public."user".id and auth.users.id in org
-- membership paths across schema generations. Keep both columns populated so
-- dashboard org selection and request visibility agree after local bootstrap
-- or manual admin grants.
update public.organization_member om
set member = u.auth_user_id
from public."user" u
where om."user" = u.id
  and om.member is null;

update public.organization_member om
set "user" = u.id
from public."user" u
where om.member = u.auth_user_id
  and om."user" is null;

select set_config('\''calciforge.key_hash'\'', :'\''key_hash'\''::text, false);
select set_config('\''calciforge.dashboard_email'\'', :'\''dashboard_email'\''::text, false);
select set_config('\''calciforge.dashboard_name'\'', :'\''dashboard_name'\''::text, false);
select set_config('\''calciforge.dashboard_password_hash'\'', :'\''dashboard_password_hash'\''::text, false);
select set_config('\''calciforge.org_name'\'', :'\''org_name'\''::text, false);

do $$
declare
  service_user_id uuid := '\''00000000-0000-4000-8000-000000000001'\'';
  service_org_id uuid := '\''00000000-0000-4000-8000-000000000002'\'';
  key_hash text := current_setting('\''calciforge.key_hash'\'', true);
  dashboard_email text := nullif(current_setting('\''calciforge.dashboard_email'\'', true), '\'''\'');
  dashboard_name text := nullif(current_setting('\''calciforge.dashboard_name'\'', true), '\'''\'');
  dashboard_password_hash text := nullif(current_setting('\''calciforge.dashboard_password_hash'\'', true), '\'''\'');
  local_org_name text := coalesce(nullif(current_setting('\''calciforge.org_name'\'', true), '\'''\'') , '\''Calciforge Local'\'');
  public_user_id text;
  auth_user_id uuid;
  target_org_id uuid;
begin
  if dashboard_email is not null then
    select u.id, u.auth_user_id into public_user_id, auth_user_id
    from public."user" u
    where lower(u.email) = lower(dashboard_email)
    limit 1;

    if public_user_id is null and dashboard_password_hash is null then
      dashboard_email := null;
    elsif public_user_id is null then
      public_user_id := '\''calciforge-'\'' || substr(md5(lower(dashboard_email)), 1, 24);
      insert into public."user" (id, name, email, "emailVerified", "createdAt", "updatedAt")
      values (public_user_id, coalesce(dashboard_name, dashboard_email), dashboard_email, true, now(), now())
      returning public."user".auth_user_id into auth_user_id;
    else
      update public."user"
      set name = coalesce(dashboard_name, name),
          "emailVerified" = true,
          "updatedAt" = now()
      where id = public_user_id
      returning public."user".auth_user_id into auth_user_id;
    end if;

    if dashboard_email is not null and dashboard_password_hash is not null then
      update public.account
      set password = dashboard_password_hash,
          "updatedAt" = now()
      where "userId" = public_user_id
        and "providerId" = '\''credential'\'';

      if not found then
        insert into public.account (id, "accountId", "providerId", "userId", password, "createdAt", "updatedAt")
        values ('\''calciforge-account-'\'' || substr(md5(lower(dashboard_email)), 1, 16), public_user_id, '\''credential'\'', public_user_id, dashboard_password_hash, now(), now());
      end if;
    end if;

    if dashboard_email is not null then
      select o.id into target_org_id
      from public.organization o
      where o.owner = auth_user_id
      order by o.is_main_org desc nulls last, o.created_at asc
      limit 1;

      if target_org_id is null then
        insert into public.organization (id, name, owner, is_personal, created_at)
        values (gen_random_uuid(), local_org_name, auth_user_id, true, now())
        returning id into target_org_id;
      end if;

      update public.organization
      set name = local_org_name
      where id = target_org_id
        and name = '\''My Organization'\'';

      update public.organization_member
      set member = auth_user_id,
          "user" = public_user_id,
          org_role = case when org_role = '\''owner'\'' then org_role else '\''admin'\'' end
      where organization = target_org_id
        and (member = auth_user_id or "user" = public_user_id);

      if not found then
        insert into public.organization_member (id, member, "user", organization, org_role, created_at)
        values (gen_random_uuid(), auth_user_id, public_user_id, target_org_id, '\''owner'\'', now());
      end if;
    end if;
  end if;

  if dashboard_email is null then
    target_org_id := service_org_id;
    auth_user_id := service_user_id;
  end if;

  update public.helicone_api_keys
  set api_key_hash = key_hash,
      api_key_name = '\''Calciforge local gateway'\'',
      user_id = auth_user_id,
      organization_id = target_org_id,
      key_permissions = '\''rw'\''
  where api_key_hash = key_hash
     or id = 1000000001;

  if not found then
    insert into public.helicone_api_keys (id, api_key_hash, api_key_name, user_id, organization_id, key_permissions, created_at)
    values (1000000001, key_hash, '\''Calciforge local gateway'\'', auth_user_id, target_org_id, '\''rw'\'', now());
  end if;
end $$;
SQL
' || warn "Could not seed local Helicone API key; dashboard login/routing metadata may need manual setup"
}

run_helicone_clickhouse_migrations() {
    truthy "$CALCIFORGE_HELICONE_DASHBOARD_ENABLED" || return 0
    command -v docker >/dev/null 2>&1 || return 0
    docker ps --format '{{.Names}}' | grep -qx "$CALCIFORGE_HELICONE_CONTAINER" || return 0

    for _ in {1..30}; do
        if docker exec "$CALCIFORGE_HELICONE_CONTAINER" sh -lc \
            'curl -fsS http://localhost:8123/ping >/dev/null 2>&1'; then
            break
        fi
        sleep 1
    done

docker exec "$CALCIFORGE_HELICONE_CONTAINER" sh -lc '
if [ -f /app/clickhouse/ch_hcone.py ]; then
  python3 /app/clickhouse/ch_hcone.py --upgrade --no-password --host localhost --port 8123 --skip-confirmation
else
  echo "Helicone ClickHouse migration helper not found" >&2
  exit 1
fi
' >/dev/null || warn "Could not run Helicone ClickHouse migrations; dashboard request history may be empty"
}

patch_helicone_dashboard_runtime_urls() {
    truthy "$CALCIFORGE_HELICONE_DASHBOARD_ENABLED" || return 0
    command -v docker >/dev/null 2>&1 || return 0
    docker ps --format '{{.Names}}' | grep -qx "$CALCIFORGE_HELICONE_CONTAINER" || return 0

    local ui_host dashboard_url jawn_url s3_url
    ui_host="$CALCIFORGE_HELICONE_DASHBOARD_BIND"
    if [[ "$ui_host" == "0.0.0.0" || "$ui_host" == "::" ]]; then
        ui_host="$(calciforge_lan_ip)"
        [[ -n "$ui_host" ]] || ui_host="127.0.0.1"
    fi
    dashboard_url="http://${ui_host}:${CALCIFORGE_HELICONE_DASHBOARD_PORT}"
    jawn_url="http://${ui_host}:${CALCIFORGE_HELICONE_JAWN_PORT}"
    s3_url="http://${ui_host}:${CALCIFORGE_HELICONE_S3_PORT}"

    docker exec \
        -e CALCIFORGE_HELICONE_DASHBOARD_URL="$dashboard_url" \
        -e CALCIFORGE_HELICONE_JAWN_URL="$jawn_url" \
        -e CALCIFORGE_HELICONE_S3_URL="$s3_url" \
        "$CALCIFORGE_HELICONE_CONTAINER" python3 - <<'PY'
import json
import os
from pathlib import Path

dashboard_url = os.environ["CALCIFORGE_HELICONE_DASHBOARD_URL"]
jawn_url = os.environ["CALCIFORGE_HELICONE_JAWN_URL"]
s3_url = os.environ["CALCIFORGE_HELICONE_S3_URL"]

supervisor_path = Path("/etc/supervisor/conf.d/supervisord.conf")
if supervisor_path.exists():
    text = supervisor_path.read_text()
    replacements = {
        'NEXT_PUBLIC_HELICONE_JAWN_SERVICE="http://localhost:8585"': f'NEXT_PUBLIC_HELICONE_JAWN_SERVICE="{jawn_url}"',
        'NEXT_PUBLIC_HELICONE_JAWN_SERVICE="http://127.0.0.1:8585"': f'NEXT_PUBLIC_HELICONE_JAWN_SERVICE="{jawn_url}"',
        'S3_ENDPOINT="http://localhost:9080"': f'S3_ENDPOINT="{s3_url}"',
        'S3_ENDPOINT="http://127.0.0.1:9080"': f'S3_ENDPOINT="{s3_url}"',
        'BETTER_AUTH_URL="http://localhost:3000"': f'BETTER_AUTH_URL="{dashboard_url}"',
        'NEXT_PUBLIC_APP_URL="http://localhost:3000"': f'NEXT_PUBLIC_APP_URL="{dashboard_url}"',
    }
    for old, new in replacements.items():
        text = text.replace(old, new)
    supervisor_path.write_text(text)

for path in [Path("/app/web/public/__ENV.js"), Path("/app/node_modules/helicone/public/__ENV.js")]:
    if not path.exists():
        continue
    raw = path.read_text().strip()
    prefix = "window.__ENV = "
    if not raw.startswith(prefix):
        continue
    body = raw[len(prefix):].strip()
    if body.endswith(";"):
        body = body[:-1]
    env = json.loads(body)
    env["NEXT_PUBLIC_APP_URL"] = dashboard_url
    env["NEXT_PUBLIC_HELICONE_JAWN_SERVICE"] = jawn_url
    env["NEXT_PUBLIC_BETTER_AUTH"] = "true"
    env["S3_ENDPOINT"] = s3_url
    path.write_text(prefix + json.dumps(env, separators=(",", ":")) + ";\n")
PY

    docker exec "$CALCIFORGE_HELICONE_CONTAINER" supervisorctl restart web jawn >/dev/null 2>&1 || true
}

ensure_helicone() {
    truthy "$CALCIFORGE_HELICONE_ENABLED" || return 0
    ensure_helicone_key

    local install_dir gateway_config gateway_bin ui_host dashboard_url jawn_url s3_url dashboard_started
    install_dir="$(helicone_install_dir)"
    gateway_config="$CALCIFORGE_CONFIG_HOME/helicone-ai-gateway.yaml"
    dashboard_started=false
    mkdir -p "$install_dir" "$ZC_LOG_DIR"

    if truthy "$CALCIFORGE_HELICONE_DASHBOARD_ENABLED"; then
        if command -v docker >/dev/null 2>&1; then
            ui_host="$CALCIFORGE_HELICONE_DASHBOARD_BIND"
            if [[ "$ui_host" == "0.0.0.0" || "$ui_host" == "::" ]]; then
                ui_host="$(calciforge_lan_ip)"
                [[ -n "$ui_host" ]] || ui_host="127.0.0.1"
            fi
            dashboard_url="http://${ui_host}:${CALCIFORGE_HELICONE_DASHBOARD_PORT}"
            jawn_url="http://${ui_host}:${CALCIFORGE_HELICONE_JAWN_PORT}"
            s3_url="http://${ui_host}:${CALCIFORGE_HELICONE_S3_PORT}"
            local better_auth_secret dashboard_bind
            better_auth_secret="$(random_hex 32 'Helicone dashboard auth secret')"
            dashboard_bind="$(docker_publish_bind "$CALCIFORGE_HELICONE_DASHBOARD_BIND")"
            docker rm -f "$CALCIFORGE_HELICONE_CONTAINER" >/dev/null 2>&1 || true
            docker run -d --name "$CALCIFORGE_HELICONE_CONTAINER" \
                --restart unless-stopped \
                -e "NEXT_PUBLIC_IS_ON_PREM=true" \
                -e "NEXT_PUBLIC_APP_URL=${dashboard_url}" \
                -e "SITE_URL=${dashboard_url}" \
                -e "BETTER_AUTH_URL=${dashboard_url}" \
                -e "BETTER_AUTH_SECRET=${better_auth_secret}" \
                -e "NEXT_PUBLIC_BETTER_AUTH=true" \
                -e "NEXT_PUBLIC_HELICONE_JAWN_SERVICE=${jawn_url}" \
                -e "S3_ENDPOINT=${s3_url}" \
                -p "${dashboard_bind}:${CALCIFORGE_HELICONE_DASHBOARD_PORT}:3000" \
                -p "${dashboard_bind}:${CALCIFORGE_HELICONE_JAWN_PORT}:8585" \
                -p "${dashboard_bind}:${CALCIFORGE_HELICONE_S3_PORT}:9080" \
                -v calciforge-helicone-postgres:/var/lib/postgresql/data \
                -v calciforge-helicone-clickhouse:/var/lib/clickhouse \
                -v calciforge-helicone-minio:/data \
                "$CALCIFORGE_HELICONE_IMAGE" >/dev/null
            dashboard_started=true
            ok "Helicone dashboard running at ${dashboard_url}"
            sleep 3
            patch_helicone_dashboard_runtime_urls
            run_helicone_clickhouse_migrations
            seed_helicone_api_key
        else
            warn "Docker not found; skipping local Helicone dashboard container"
        fi
    fi

    if [[ ! -d "$install_dir/node_modules/@helicone/ai-gateway" ]]; then
        require_npm
        ( cd "$install_dir" && npm init -y >/dev/null 2>&1 && npm install "$CALCIFORGE_HELICONE_PACKAGE" >/dev/null )
    fi
    gateway_bin="$(helicone_ai_gateway_bin "$install_dir")"
    [[ -x "$gateway_bin" ]] || die "Helicone AI Gateway binary not found under $install_dir"
    write_helicone_ai_gateway_config "$gateway_config"

    if [[ "$PLATFORM" == "Darwin" ]]; then
        local plist="$PLIST_DIR/com.calciforge.helicone-ai-gateway.plist"
        mkdir -p "$PLIST_DIR"
        cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>Label</key><string>com.calciforge.helicone-ai-gateway</string>
    <key>ProgramArguments</key><array>
        <string>${gateway_bin}</string>
        <string>--config</string>
        <string>${gateway_config}</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>${ZC_LOG_DIR}/helicone-ai-gateway.log</string>
    <key>StandardErrorPath</key><string>${ZC_LOG_DIR}/helicone-ai-gateway.err</string>
</dict></plist>
EOF
        load_launch_agent "com.calciforge.helicone-ai-gateway" "$plist"
    else
        local unit="$PLIST_DIR/calciforge-helicone-ai-gateway.service"
        local systemd_gateway_bin systemd_gateway_config
        systemd_gateway_bin="$(systemd_exec_arg "$gateway_bin")"
        systemd_gateway_config="$(systemd_exec_arg "$gateway_config")"
        cat > "$unit" <<EOF
[Unit]
Description=Calciforge Helicone AI Gateway
After=network.target
Wants=network.target

[Service]
Type=simple
ExecStart=${systemd_gateway_bin} --config ${systemd_gateway_config}
Restart=always
RestartSec=5
StandardOutput=append:${ZC_LOG_DIR}/helicone-ai-gateway.log
StandardError=append:${ZC_LOG_DIR}/helicone-ai-gateway.err

[Install]
WantedBy=${WANTED_BY_TARGET}
EOF
        $SYSTEMCTL daemon-reload
        enable_restart_service calciforge-helicone-ai-gateway.service || warn "Could not start Helicone AI Gateway service"
    fi

    CALCIFORGE_GATEWAY_BACKEND_TYPE="helicone"
    CALCIFORGE_GATEWAY_BACKEND_URL="http://127.0.0.1:${CALCIFORGE_HELICONE_AI_GATEWAY_PORT}/ai"
    CALCIFORGE_GATEWAY_BACKEND_API_KEY_FILE="$CALCIFORGE_HELICONE_API_KEY_FILE"
    if [[ -z "$CALCIFORGE_GATEWAY_UI_URL" ]] && truthy "$dashboard_started"; then
        CALCIFORGE_GATEWAY_UI_URL="$dashboard_url"
    fi
}
