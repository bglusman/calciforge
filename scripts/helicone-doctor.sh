#!/usr/bin/env bash
# Check the local Helicone stack that Calciforge provisions for gateway
# observability. This intentionally checks the browser-visible dashboard path,
# not only the gateway write path.

set -euo pipefail

CONTAINER="${CALCIFORGE_HELICONE_CONTAINER:-calciforge-helicone}"
EMAIL="${CALCIFORGE_HELICONE_DASHBOARD_USER_EMAIL:-${1:-}}"
DB="${CALCIFORGE_HELICONE_DB:-helicone_test}"
PASSWORD="${CALCIFORGE_HELICONE_DASHBOARD_PASSWORD:-}"
PASSWORD_FILE="${CALCIFORGE_HELICONE_DASHBOARD_PASSWORD_FILE:-}"
REQUIRE_VISIBLE_ROWS="${CALCIFORGE_HELICONE_REQUIRE_VISIBLE_ROWS:-false}"

failures=0

ok() { printf 'ok: %s\n' "$*"; }
warn() { printf 'warn: %s\n' "$*" >&2; }
fail() { printf 'error: %s\n' "$*" >&2; failures=$((failures + 1)); }
truthy() { [[ "$1" == "1" || "$1" == "true" || "$1" == "yes" || "$1" == "on" ]]; }
json_env_value() {
    python3 -c 'import json, re, sys
key = sys.argv[1]
m = re.search(r"window\.__ENV\s*=\s*(\{.*\})\s*;?", sys.stdin.read())
print(json.loads(m.group(1)).get(key, "") if m else "")' "$1"
}
sql_literal() {
    python3 -c 'import sys
print("'"'"'" + sys.argv[1].replace("'"'"'", "'"'"''"'"'") + "'"'"'")' "$1"
}

require_container() {
    if ! docker ps --format '{{.Names}}' | grep -qx "$CONTAINER"; then
        fail "container '$CONTAINER' is not running"
        exit 1
    fi
    ok "container '$CONTAINER' is running"
}

container_env() {
    docker exec "$CONTAINER" sh -lc "printf '%s\n' \"\${$1:-}\""
}

read_dashboard_password() {
    if [[ -n "$PASSWORD" ]]; then
        printf '%s' "$PASSWORD"
    elif [[ -n "$PASSWORD_FILE" && -s "$PASSWORD_FILE" ]]; then
        tr -d '\n' < "$PASSWORD_FILE"
    fi
}

check_env_and_ports() {
    local app_url jawn_url s3_url
    app_url="$(container_env NEXT_PUBLIC_APP_URL)"
    jawn_url="$(container_env NEXT_PUBLIC_HELICONE_JAWN_SERVICE)"
    s3_url="$(container_env S3_ENDPOINT)"

    [[ -n "$app_url" ]] && ok "dashboard URL: $app_url" || fail "NEXT_PUBLIC_APP_URL is empty"
    [[ -n "$jawn_url" ]] && ok "browser Jawn URL: $jawn_url" || fail "NEXT_PUBLIC_HELICONE_JAWN_SERVICE is empty"
    [[ -n "$s3_url" ]] && ok "S3 endpoint: $s3_url" || warn "S3_ENDPOINT is empty"

    if [[ "$app_url" =~ ^http://([^/:]+):([0-9]+) ]]; then
        local app_host="${BASH_REMATCH[1]}"
        if [[ "$app_host" != "127.0.0.1" && "$app_host" != "localhost" && "$jawn_url" =~ ^http://127\.0\.0\.1: ]]; then
            fail "LAN dashboard advertises loopback Jawn URL; remote browsers will show no data"
        fi
    fi

    docker port "$CONTAINER" 3000/tcp >/dev/null 2>&1 && ok "dashboard port is published" || fail "dashboard port 3000 is not published"
    docker port "$CONTAINER" 8585/tcp >/dev/null 2>&1 && ok "Jawn port is published" || fail "Jawn port 8585 is not published"

    if command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1 && [[ -n "$app_url" ]]; then
        local public_env public_jawn public_s3
        public_env="$(curl -fsS "${app_url%/}/__ENV.js" 2>/dev/null || true)"
        if [[ -z "$public_env" ]]; then
            fail "dashboard __ENV.js is not reachable at ${app_url%/}/__ENV.js"
        else
            public_jawn="$(printf '%s' "$public_env" | json_env_value NEXT_PUBLIC_HELICONE_JAWN_SERVICE 2>/dev/null || true)"
            public_s3="$(printf '%s' "$public_env" | json_env_value S3_ENDPOINT 2>/dev/null || true)"
            [[ -n "$public_jawn" ]] && ok "dashboard browser Jawn URL from __ENV.js: $public_jawn" || fail "dashboard __ENV.js does not expose NEXT_PUBLIC_HELICONE_JAWN_SERVICE"
            if [[ "$app_url" =~ ^http://([^/:]+):([0-9]+) ]]; then
                local app_host="${BASH_REMATCH[1]}"
                if [[ "$app_host" != "127.0.0.1" && "$app_host" != "localhost" && -z "$public_s3" ]]; then
                    fail "dashboard __ENV.js does not expose S3_ENDPOINT; remote browsers may not load request bodies"
                fi
                if [[ "$app_host" != "127.0.0.1" && "$app_host" != "localhost" && ( "$public_jawn" =~ ^http://127\.0\.0\.1: || "$public_jawn" =~ ^http://localhost: ) ]]; then
                    fail "dashboard __ENV.js advertises loopback Jawn URL; remote browsers will show no data"
                fi
                if [[ "$app_host" != "127.0.0.1" && "$app_host" != "localhost" && ( "$public_s3" =~ ^http://127\.0\.0\.1: || "$public_s3" =~ ^http://localhost: ) ]]; then
                    fail "dashboard __ENV.js advertises loopback S3 URL; request bodies will not load remotely"
                fi
            fi
        fi
    fi
}

psql_query() {
    docker exec "$CONTAINER" psql "postgresql://postgres:password@localhost:5432/$DB" -Atc "$1"
}

clickhouse_query() {
    docker exec "$CONTAINER" clickhouse-client --query "$1"
}

check_user_and_key() {
    local email_filter="" email_sql=""
    if [[ -n "$EMAIL" ]]; then
        email_sql="$(sql_literal "$EMAIL")"
        email_filter="where u.email = $email_sql"
    fi

    local user_rows
    user_rows="$(psql_query "select count(*) from public.\"user\" u $email_filter;")" || {
        fail "could not query Helicone users"
        return
    }
    [[ "$user_rows" -gt 0 ]] && ok "dashboard user exists (${EMAIL:-any email})" || fail "dashboard user not found: ${EMAIL:-no email supplied}"

    if [[ -n "$EMAIL" ]]; then
        local credential_rows owner_rows
        credential_rows="$(psql_query "select count(*) from public.\"user\" u join public.account a on a.\"userId\" = u.id where u.email = $email_sql and a.\"providerId\" = 'credential' and length(coalesce(a.password, '')) > 0;")"
        owner_rows="$(psql_query "select count(*) from public.\"user\" u join public.organization_member om on om.member = u.auth_user_id or om.\"user\" = u.id where u.email = $email_sql and om.org_role in ('owner', 'admin');")"
        [[ "$credential_rows" -gt 0 ]] && ok "dashboard credential password is present" || fail "dashboard credential password is missing for $EMAIL"
        [[ "$owner_rows" -gt 0 ]] && ok "dashboard user has owner/admin org membership" || fail "dashboard user lacks owner/admin org membership"
    fi

    local rw_keys
    rw_keys="$(psql_query "select count(*) from public.helicone_api_keys where api_key_name = 'Calciforge local gateway' and key_permissions like '%r%' and key_permissions like '%w%' and soft_delete = false;")"
    [[ "$rw_keys" -gt 0 ]] && ok "Calciforge local gateway API key has read/write permissions" || fail "Calciforge local gateway API key is missing read/write permissions"

    if [[ -n "$EMAIL" ]]; then
        local visible_key_rows
        visible_key_rows="$(psql_query "select count(*) from public.helicone_api_keys k join public.\"user\" u on lower(u.email) = lower($email_sql) join public.organization_member om on (om.member = u.auth_user_id or om.\"user\" = u.id) and om.organization = k.organization_id where k.api_key_name = 'Calciforge local gateway' and k.key_permissions like '%r%' and k.key_permissions like '%w%' and k.soft_delete = false;")"
        [[ "$visible_key_rows" -gt 0 ]] \
            && ok "Calciforge local gateway API key belongs to a $EMAIL organization" \
            || fail "Calciforge local gateway API key is not attached to a $EMAIL organization; dashboard may show no traffic"
    fi
}

check_request_visibility() {
    clickhouse_query "exists default.request_response_rmt" | grep -qx 1 \
        && ok "ClickHouse request_response_rmt table exists" \
        || { fail "ClickHouse request_response_rmt table is missing"; return; }

    local rows
    rows="$(clickhouse_query "select count() from default.request_response_rmt")" || {
        fail "could not query ClickHouse request_response_rmt"
        return
    }
    [[ "$rows" -gt 0 ]] && ok "ClickHouse has $rows request row(s)" || warn "ClickHouse has no request rows yet"

    if [[ -n "$EMAIL" ]]; then
        local email_sql orgs org_filter org_rows
        email_sql="$(sql_literal "$EMAIL")"
        orgs="$(psql_query "select string_agg(quote_literal(om.organization::text), ',') from public.\"user\" u join public.organization_member om on om.member = u.auth_user_id or om.\"user\" = u.id where u.email = $email_sql;")"
        if [[ -n "$orgs" ]]; then
            org_filter="$(printf '%s' "$orgs" | tr -d "'")"
            org_rows="$(clickhouse_query "select count() from default.request_response_rmt where organization_id in (${orgs})")" || org_rows=0
            if [[ "$org_rows" -gt 0 ]]; then
                ok "ClickHouse has $org_rows request row(s) visible to $EMAIL org(s): $org_filter"
            elif [[ "$rows" -gt 0 ]]; then
                fail "ClickHouse has request rows, but none are visible to $EMAIL org(s): $org_filter"
            elif truthy "$REQUIRE_VISIBLE_ROWS"; then
                fail "ClickHouse has no rows for $EMAIL org(s): $org_filter"
            else
                warn "ClickHouse has no rows for $EMAIL org(s): $org_filter"
            fi
        fi
    fi
}

check_dashboard_request_api_visibility() {
    [[ -n "$EMAIL" ]] || return 0
    command -v curl >/dev/null 2>&1 || { warn "curl not found; skipping dashboard API visibility check"; return 0; }
    command -v python3 >/dev/null 2>&1 || { warn "python3 not found; skipping dashboard API visibility check"; return 0; }

    local password
    password="$(read_dashboard_password)"
    if [[ -z "$password" ]]; then
        warn "dashboard password not supplied; skipping logged-in dashboard API visibility check"
        return 0
    fi

    local app_url jawn_url org_id org_rows jar auth response data_count email_sql
    app_url="$(container_env NEXT_PUBLIC_APP_URL)"
    jawn_url="$(curl -fsS "${app_url%/}/__ENV.js" 2>/dev/null | json_env_value NEXT_PUBLIC_HELICONE_JAWN_SERVICE 2>/dev/null || true)"
    [[ -n "$jawn_url" ]] || { fail "could not read browser Jawn URL from dashboard __ENV.js"; return; }
    email_sql="$(sql_literal "$EMAIL")"

    org_id="$(psql_query "select k.organization_id::text from public.helicone_api_keys k join public.\"user\" u on lower(u.email) = lower($email_sql) join public.organization_member om on (om.member = u.auth_user_id or om.\"user\" = u.id) and om.organization = k.organization_id where k.api_key_name = 'Calciforge local gateway' and k.key_permissions like '%r%' and k.key_permissions like '%w%' and k.soft_delete = false order by k.created_at desc limit 1;")"
    if [[ -z "$org_id" ]]; then
        fail "could not find a $EMAIL-visible organization for the Calciforge local gateway API key"
        return
    fi

    org_rows="$(clickhouse_query "select count() from default.request_response_rmt where organization_id = '${org_id}'")" || org_rows=0
    if [[ "$org_rows" -eq 0 ]] && ! truthy "$REQUIRE_VISIBLE_ROWS"; then
        warn "no ClickHouse rows exist for gateway organization $org_id; dashboard API visibility check has no data to prove"
        return 0
    fi

    jar="$(mktemp)"
    if ! curl -fsS -c "$jar" -b "$jar" -H 'Content-Type: application/json' \
        "${app_url%/}/api/auth/sign-in/email" \
        --data "$(python3 -c 'import json, os, sys; print(json.dumps({"email": sys.argv[1], "password": sys.argv[2]}))' "$EMAIL" "$password")" >/dev/null; then
        rm -f "$jar"
        fail "could not sign in to dashboard as $EMAIL"
        return
    fi

    auth="$(python3 -c 'import json, sys; print(json.dumps({"_type": "jwt", "token": "calciforge-doctor", "orgId": sys.argv[1]}))' "$org_id")"
    response="$(curl -fsS -b "$jar" \
        -H "helicone-authorization: $auth" \
        -H 'Content-Type: application/json' \
        "${jawn_url%/}/v1/request/query-clickhouse" \
        --data '{"filter":"all","offset":0,"limit":5,"sort":{"created_at":"desc"},"isCached":false}' 2>/dev/null || true)"
    rm -f "$jar"
    if [[ -z "$response" ]]; then
        fail "dashboard browser Jawn request list API is not reachable at ${jawn_url%/}/v1/request/query-clickhouse"
        return
    fi
    data_count="$(printf '%s' "$response" | python3 -c 'import json, sys; data=json.load(sys.stdin); print(len(data.get("data") or []));' 2>/dev/null || echo 0)"
    if [[ "$data_count" -gt 0 ]]; then
        ok "dashboard request API returns $data_count visible request row(s) for $EMAIL gateway org $org_id"
    else
        fail "dashboard request API returned no rows for $EMAIL gateway org $org_id even though ClickHouse has $org_rows row(s)"
    fi
}

require_container
check_env_and_ports
check_user_and_key
check_request_visibility
check_dashboard_request_api_visibility

if [[ "$failures" -gt 0 ]]; then
    printf '\nHelicone doctor failed with %s error(s).\n' "$failures" >&2
    exit 1
fi

printf '\nHelicone doctor passed.\n'
