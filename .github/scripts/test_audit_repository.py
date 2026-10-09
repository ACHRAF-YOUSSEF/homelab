"""Focused fixtures for the privacy guard; fixtures contain no real credentials."""

import contextlib
import io
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import audit_repository as audit


class ContentTests(unittest.TestCase):
    def rules(self, findings):
        return {finding.rule for finding in findings}

    def test_private_ranges_rejected_but_loopback_and_documentation_ips_allowed(self):
        findings = audit.audit_markdown("README.md", "10.1.2.3\n172.16.1.2\n192.168.1.2\n")
        self.assertEqual([finding.line for finding in findings], [1, 2, 3])
        self.assertFalse(audit.audit_markdown("README.md", "http://localhost:7575\n127.0.0.1\n192.0.2.10\n172.15.1.2\n"))

    def test_real_assignments_and_private_key_markers_rejected(self):
        key_header = "-----BEGIN OPENSSH " + "PRIVATE KEY-----"
        text = 'API_KEY=fixture-sensitive-value\n$env:MY_PASSWORD="fixture-sensitive-value"\nAuthorization: Bearer fixture-sensitive-value\n' + key_header + '\n'
        findings = audit.audit_markdown("README.md", text)
        self.assertEqual(len(findings), 4)
        self.assertIn("markdown-private-key", self.rules(findings))
        self.assertTrue(all("fixture-sensitive-value" not in str(finding) for finding in findings))

    def test_placeholders_and_local_api_dummy_keys_allowed(self):
        text = '\n'.join((
            'API_KEY=', 'API_KEY=""', 'API_KEY=${MY_API_KEY}',
            'API_KEY=<your-api-key>', 'TOKEN=your_token_here',
            'PASSWORD=example-password', 'OPENAI_API_KEY=lmstudio',
            'Authorization: Bearer ${MY_API_KEY}',
        ))
        self.assertFalse(audit.audit_markdown("README.md", text))
        self.assertTrue(audit.audit_markdown("README.md", 'TOKEN="${MY_TOKEN}fixture-sensitive-value"'))

    def test_example_credentials_must_be_blank_even_for_placeholders(self):
        safe = 'GLANCE_DOCKER_HOST=tcp://docker-proxy:2375\nAPI_KEY=""\nYT_TA_USERNAME=\n'
        self.assertFalse(audit.audit_env_example(safe))
        findings = audit.audit_env_example(safe + 'API_KEY=your_key_here\nPASSWORD="fixture-sensitive-value"\n')
        self.assertEqual(len(findings), 2)
        self.assertEqual(self.rules(findings), {"example-credential-not-blank"})

    def test_example_docker_endpoint_must_use_proxy(self):
        self.assertIn("example-docker-client-not-proxy", self.rules(audit.audit_env_example('GLANCE_DOCKER_HOST=tcp://localhost:2375\n')))
        self.assertIn("example-docker-proxy-setting-missing", self.rules(audit.audit_env_example('API_KEY=\n')))

    def test_hash_prefixed_credentials_and_json_assignments_rejected(self):
        example = 'GLANCE_DOCKER_HOST=tcp://docker-proxy:2375\nAPI_KEY=#fixture-sensitive-value\n'
        self.assertIn("example-credential-not-blank", self.rules(audit.audit_env_example(example)))
        findings = audit.audit_markdown("README.md", '{"API_KEY": "fixture-sensitive-value"}')
        self.assertIn("markdown-credential-assignment", self.rules(findings))

    def test_glance_env_endpoints_templates_and_relative_assets_allowed(self):
        text = '\n'.join((
            'url: ${GLANCE_GITHUB_URL}/search?q={QUERY}',
            'sock-path: ${GLANCE_DOCKER_HOST}',
            'icon: /assets/subtitle-studio.svg',
            '  <a href="${HOMELAB_URL}:5055/requests">Requests</a>',
            '  <img href="/assets/example.svg">',
        ))
        self.assertFalse(audit.audit_glance_yaml("glance/config/pages/tools.yml", text))

    def test_glance_literal_urls_ips_domains_and_wrong_proxy_rejected(self):
        text = '\n'.join((
            'url: https://example.invalid/',
            'check-url: 192.0.2.10',
            'url: example.invalid/feed',
            'address: "::1"',
            'sock-path: ${OTHER_DOCKER_HOST}',
            '  <a href="example.invalid/path">Link</a>',
        ))
        rules = self.rules(audit.audit_glance_yaml("glance/config/pages/tools.yml", text))
        self.assertEqual(rules, {"glance-literal-url", "glance-literal-ip", "glance-endpoint-not-env", "glance-docker-client-not-env-proxy", "glance-href-not-env"})

    def test_quoted_yaml_keys_and_flow_mapping_endpoints_checked(self):
        safe = '{"url": "${HOMELAB_URL}", "sock-path": "${GLANCE_DOCKER_HOST}"}'
        self.assertFalse(audit.audit_glance_yaml("glance/config/glance.yml", safe))
        findings = audit.audit_glance_yaml("glance/config/glance.yml", '{url: ${HOMELAB_URL}, "check-url": "example.invalid"}')
        self.assertIn("glance-endpoint-not-env", self.rules(findings))


@unittest.skipUnless(shutil.which("git"), "Repository fixtures require Git")
class RepositoryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.run_git("init", "--quiet")
        self.write(".gitignore", ".env\n")
        self.write(".env.example", "GLANCE_DOCKER_HOST=tcp://docker-proxy:2375\nAPI_KEY=\n")
        self.write("README.md", "# Example\nUse http://localhost:7575\n")
        self.run_git("add", ".gitignore", ".env.example", "README.md")

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def run_git(self, *args):
        subprocess.run(["git", "-C", str(self.root), *args], capture_output=True, check=True)

    def test_clean_repository_and_svg_namespace_are_allowed(self):
        self.write("glance/config/glance.yml", "app-name: ${GLANCE_APP_NAME}\n")
        self.write("glance/config/assets/icon.svg", '<svg xmlns="http://www.w3.org/2000/svg"/>')
        self.assertFalse(audit.audit_repository(self.root))

    def test_untracked_glance_yaml_and_markdown_are_checked(self):
        self.write("glance/config/pages/new.yaml", "url: https://example.invalid/\n")
        self.write("AGENTS.md", "Private server: 10.1.2.3\n")
        rules = {finding.rule for finding in audit.audit_repository(self.root)}
        self.assertIn("glance-literal-url", rules)
        self.assertIn("markdown-private-ip", rules)

    def test_real_env_tracked_and_missing_ignore_are_rejected(self):
        self.write(".env", "API_KEY=fixture-sensitive-value\n")
        self.run_git("add", "--force", ".env")
        self.write(".gitignore", "")
        rules = {finding.rule for finding in audit.audit_repository(self.root)}
        self.assertIn("real-env-tracked", rules)
        self.assertIn("real-env-not-ignored", rules)

    def test_local_env_never_read_or_misreported_as_tracked(self):
        self.write(".env", "API_KEY=fixture-sensitive-value\n")
        self.assertFalse(audit.audit_repository(self.root))
        self.write(".gitignore", "")
        rules = {finding.rule for finding in audit.audit_repository(self.root)}
        self.assertEqual(rules, {"real-env-not-ignored"})

    def test_cli_has_failure_exit_code_and_never_displays_values(self):
        self.write("README.md", "# Fixture\nAPI_KEY=fixture-sensitive-value\n")
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            status = audit.main(["--root", str(self.root)])
        self.assertEqual(status, 1)
        self.assertIn("README.md:2: markdown-credential-assignment", output.getvalue())
        self.assertNotIn("fixture-sensitive-value", output.getvalue())


if __name__ == "__main__":
    unittest.main()
