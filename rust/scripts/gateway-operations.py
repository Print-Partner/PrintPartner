import argparse
import hashlib
import json
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser()
parser.add_argument('inventory', type=pathlib.Path)
parser.add_argument('--check', action='store_true')
parser.add_argument('--self-test', action='store_true')
args = parser.parse_args()
inventory = json.loads(args.inventory.read_text())
# The desktop child runs the single-user self-host build. Routes Fastify only
# registers in other modes (board, shares) are absent there, not admitted.
DESKTOP_MODE = 'self-host/single-user-auth'
registered = [route for route in inventory['routes']
              if route.get('registeredByMode', {}).get(DESKTOP_MODE, {'registered': True})['registered']]
GATEWAY_OWNED = [{'method': 'POST', 'path': '/auth/logout', 'reason': 'Revokes the desktop session locally'}]


def normalize_owner(value):
    if value == 'compat':
        return 'node'
    return value


def planted_ownership_error(document):  # noqa: C901
    """Reject worker, startup, and MCP ownership the route table cannot see."""
    section_keys = ('workers', 'startup', 'mcp', 'operations')
    if not any(key in document for key in section_keys):
        return None
    participants = []
    for kind, key in (('worker', 'workers'), ('startup_watcher', 'startup'), ('mcp_action', 'mcp')):
        entries = document.get(key, [])
        if not isinstance(entries, list):
            return f'ownership admission: {key} must be an array'
        for entry in entries:
            if not isinstance(entry, dict):
                return 'ownership admission: malformed ownership manifest'
            participants.append({
                'kind': kind,
                'owner': normalize_owner(str(entry.get('owner', ''))),
                'raw_owner': str(entry.get('owner', '')),
                'operation': str(entry.get('operation', '')),
            })
    operations = document.get('operations')
    if isinstance(operations, dict):
        for name, body in operations.items():
            entries = body.get('participants') if isinstance(body, dict) else None
            if not isinstance(entries, list):
                return 'ownership admission: malformed ownership manifest'
            for entry in entries:
                if not isinstance(entry, dict):
                    return 'ownership admission: malformed ownership manifest'
                participants.append({
                    'kind': str(entry.get('kind', '')),
                    'owner': normalize_owner(str(entry.get('owner', ''))),
                    'raw_owner': str(entry.get('owner', '')),
                    'operation': str(name),
                })
    elif operations is not None:
        return 'ownership admission: malformed ownership manifest'
    if not participants:
        return 'ownership admission: ownership sections declare no participants'
    for entry in participants:
        if entry['kind'] not in {'http', 'http_alias', 'mcp_action', 'job', 'startup_watcher', 'worker', 'migration', 'assistant_action'}:
            return f"ownership admission: unknown participant kind {entry['kind']}"
        if entry['raw_owner'] not in {'node', 'compat', 'rust'}:
            return f"ownership admission: unknown owner {entry['raw_owner']}"
        if entry['operation'] != 'source.sync':
            return f"ownership admission: unknown operation {entry['operation']}"
    owners = {entry['owner'] for entry in participants}
    kinds = {entry['kind'] for entry in participants}
    if len(owners) > 1:
        return 'ownership admission: conflicting ownership for source.sync'
    if owners == {'rust'}:
        return 'ownership admission: unsupported rust owner'
    for required in ('worker', 'startup_watcher', 'mcp_action'):
        if required not in kinds:
            return f'ownership admission: unrepresented participant {required}'
    return None


ownership_error = planted_ownership_error(inventory)
if ownership_error:
    raise SystemExit(ownership_error)


def base(path):
    return re.sub(r'^/api/v[12](?=/|$)', '', path) or '/'


def denied(method, path):
    bare = base(path)
    if method == 'OPTIONS':
        return 'Cross-origin API access is disabled'
    if bare == '/mcp':
        return 'MCP requires separately verified API-key authorization'
    if path.startswith('/auth/') and path not in ['/auth/me', '/auth/logout']:
        return 'Native bootstrap owns desktop authentication; OAuth and development login are unavailable'
    if path.startswith('/api/v1/docs') or bare in ['/openapi.json', '/metrics']:
        return 'Server administration and API documentation are outside the desktop product'
    if bare.startswith('/admin/'):
        return 'Server-path administrative import is unavailable in desktop'
    if bare == '/settings/external-access' and method != 'GET' and method != 'HEAD':
        return 'LAN access remains disabled in desktop'
    if re.fullmatch(r'/slicer-instances/[^/]+/docker-[^/]+', bare):
        return 'Docker lifecycle and logs are unavailable in this native foundation'
    return None


PURE_READS = {
    '/legal/summary': 'routes/legal.ts registerLegalRoutes reads LICENSE-SUMMARY.md; Rust serves the same bundled document without mutations or provider calls',
    '/printers': 'routes/printers.ts:160 -> services/printer-fleet.ts:103 loadFleet only parses repo.getSetting',
    '/integrations': 'routes/integrations.ts:26 -> integrations/store.ts:155 list projects stored settings and adapter capabilities without calling providers',
}


def effect(method, path):
    bare = base(path)
    if method in ['GET', 'HEAD']:
        if bare == '/settings/api-keys':
            return 'local_commit', 'services/api-key-manager.ts:46 loadKeys may migrate legacy key hashes and persist settings'
        if bare == '/settings/update-check':
            return 'local_commit', 'May reset the local update-check cache; desktop disables remote update checks'
        if bare == '/printer-checkoff':
            return 'local_commit', 'Repairs incomplete accepted printer links before returning them'
        if bare in PURE_READS:
            return 'observation', PURE_READS[bare]
        return 'external_effect', 'Conservatively guarded GET/HEAD: no audited pure-read exception; provider or local effects may occur'
    external = (
        bare.startswith(('/sources', '/assistant', '/slicer-instances', '/reference-shares/imports'))
        or bare in ['/jobs/sync', '/jobs/check-source-updates', '/jobs/extract-source-docs', '/jobs/printer-upload',
                    '/api/discord-digest', '/settings/discord-notify/test', '/integrations/:id/test',
                    '/printer-checkoff/verify', '/printer-checkoff/reconcile', '/printer-checkoff/:id/recheck-remote-file',
                    '/printer-send-queue/drain', '/printer-send-queue/:id/dispatch', '/bambu-connect/handoff']
    )
    return ('external_effect', 'May start provider work, background acquisition or external delivery') if external else (
        'local_commit', 'Node owns the complete local transaction or file publication')


if args.self_test:
    corrected = {'/integrations/:id/spoolman/filaments', '/integrations/:id/spoolman/spools',
                 '/plans/:id/review', '/plans/:id/role-filaments', '/plans/:id/checkoff'}
    observed = [route for route in registered if route['method'] in ['GET', 'HEAD'] and base(route['path']) in corrected]
    assert {base(route['path']) for route in observed} == corrected
    for route in observed:
        assert effect(route['method'], route['path'])[0] == 'external_effect', route
    for method in ['GET', 'HEAD']:
        for prefix in ['', '/api/v1', '/api/v2']:
            assert effect(method, prefix + '/future/unreviewed-provider-read')[0] == 'external_effect'
            assert effect(method, prefix + '/settings/api-keys')[0] == 'local_commit'
            for pure in PURE_READS:
                classification, evidence = effect(method, prefix + pure)
                assert classification == 'observation' and evidence == PURE_READS[pure]
    print(json.dumps({'corrected_registered_reads': len(observed), 'unknown_reads_guarded': 6,
                      'pure_exception_families': len(PURE_READS)}))
    raise SystemExit(0)

routes = []
exclusions = []
for registered_route in sorted(registered, key=lambda route: (route['path'], route['method'])):
    method, path = registered_route['method'], registered_route['path']
    if any(owned['method'] == method and owned['path'] == path for owned in GATEWAY_OWNED):
        continue
    reason = denied(method, path)
    if reason:
        exclusions.append({'method': method, 'path': path, 'reason': reason})
        continue
    classification, effect_reason = effect(method, path)
    contract = 'legacy-v1' if path.startswith('/api/v1/plans') else 'current'
    operation = ('GET' if method == 'HEAD' else method) + ' ' + re.sub(r':[^/]+', ':param', base(path)) + ' ' + contract
    routes.append({'method': method, 'path': path, 'owner': 'compat', 'effect': classification,
                   'operation': operation, 'contract': contract, 'websocket': registered_route['websocket'],
                   'effect_reason': effect_reason})
normalized = json.dumps(sorted(registered, key=lambda route: (route['path'], route['method'])), sort_keys=True, separators=(',', ':'))
result = {'scope': 'Observed desktop self-host registration; Node owns domain/background/file effects',
          'registry_sha256': hashlib.sha256(normalized.encode()).hexdigest(),
          'gateway_owned': GATEWAY_OWNED,
          'routes': routes, 'denied': exclusions}
encoded = json.dumps(result, indent=2) + '\n'
output = ROOT / 'rust/crates/pp-gateway/operations.json'
if args.check:
    if output.read_text() != encoded:
        raise SystemExit('Gateway registry differs from actual registered routes and policy; regenerate and review')
else:
    output.write_text(encoded)
print(json.dumps({'registered': len(registered), 'admitted': len(routes), 'explicitly_denied': len(exclusions),
                  'registry_sha256': result['registry_sha256']}))
