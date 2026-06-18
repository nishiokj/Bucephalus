package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	modal "github.com/modal-labs/modal-client/go"
	_ "golang.org/x/crypto/x509roots/fallback"
)

const runtimeTransferArchivePath = "/tmp/bucephalus-runtime-transfer.tar.gz"

type execRecord struct {
	Phase                 string  `json:"phase,omitempty"`
	SandboxID             string  `json:"sandbox_id,omitempty"`
	ProcessID             *string `json:"process_id"`
	ExitCode              int     `json:"exit_code"`
	TimedOut              bool    `json:"timed_out"`
	StartedAt             string  `json:"started_at"`
	ContainerStartedAt    *string `json:"container_started_at"`
	AgentCommandStartedAt *string `json:"agent_command_started_at"`
	EndedAt               string  `json:"ended_at"`
}

type launchResult struct {
	SandboxID                   *string           `json:"sandbox_id"`
	Execs                       []execRecord      `json:"execs"`
	ExitCode                    *int              `json:"exit_code"`
	TimedOut                    bool              `json:"timed_out"`
	LauncherError               *string           `json:"launcher_error,omitempty"`
	StartedAt                   string            `json:"started_at"`
	EndedAt                     *string           `json:"ended_at"`
	RuntimeTransferArchiveBytes int64             `json:"runtime_transfer_archive_bytes"`
	Timings                     map[string]string `json:"timings"`
}

type readResult struct {
	data []byte
	err  error
}

func utcNow() string {
	return time.Now().UTC().Format("2006-01-02T15:04:05.000000Z")
}

func timingMark(timings map[string]string, key string) {
	timings[key] = utcNow()
}

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, format+"\n", args...)
	os.Exit(1)
}

func loadJSON(path string) (map[string]any, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var value map[string]any
	if err := json.Unmarshal(data, &value); err != nil {
		return nil, err
	}
	return value, nil
}

func jsonObject(value any) map[string]any {
	if value == nil {
		return map[string]any{}
	}
	if object, ok := value.(map[string]any); ok {
		return object
	}
	return map[string]any{}
}

func jsonObjects(value any) []map[string]any {
	items, ok := value.([]any)
	if !ok {
		return nil
	}
	out := make([]map[string]any, 0, len(items))
	for _, item := range items {
		out = append(out, jsonObject(item))
	}
	return out
}

func jsonObjectMap(value any) map[string]map[string]any {
	object, ok := value.(map[string]any)
	if !ok {
		return map[string]map[string]any{}
	}
	out := make(map[string]map[string]any, len(object))
	for key, value := range object {
		out[key] = jsonObject(value)
	}
	return out
}

func stringValue(object map[string]any, key string) string {
	value, _ := object[key].(string)
	return value
}

func optionalString(object map[string]any, key string) (string, bool) {
	value, ok := object[key].(string)
	return value, ok && value != ""
}

func boolValue(object map[string]any, key string) bool {
	value, ok := object[key].(bool)
	return ok && value
}

func intValue(object map[string]any, key string, fallback int) int {
	switch value := object[key].(type) {
	case float64:
		return int(value)
	case int:
		return value
	case json.Number:
		parsed, err := value.Int64()
		if err == nil {
			return int(parsed)
		}
	}
	return fallback
}

func stringList(value any) []string {
	switch items := value.(type) {
	case []string:
		return append([]string(nil), items...)
	case []any:
		out := make([]string, 0, len(items))
		for _, item := range items {
			if text, ok := item.(string); ok {
				out = append(out, text)
			}
		}
		return out
	default:
		return nil
	}
}

func stringMap(value any) map[string]string {
	object, ok := value.(map[string]any)
	if !ok {
		return map[string]string{}
	}
	out := make(map[string]string, len(object))
	for key, value := range object {
		if text, ok := value.(string); ok {
			out[key] = text
		} else if value != nil {
			encoded, _ := json.Marshal(value)
			out[key] = string(encoded)
		}
	}
	return out
}

func marker(prefix string, value any) {
	data, err := json.Marshal(value)
	if err != nil {
		fail("marshal %s: %v", prefix, err)
	}
	fmt.Printf("%s=%s\n", prefix, data)
}

func requiredEnv(name string) (string, error) {
	value := os.Getenv(name)
	if value == "" {
		return "", fmt.Errorf("%s is required for Modal S3-compatible sync", name)
	}
	return value, nil
}

func requiredAnyEnv(names ...string) (string, error) {
	for _, name := range names {
		if value := os.Getenv(name); value != "" {
			return value, nil
		}
	}
	return "", fmt.Errorf("%s is required for Modal S3-compatible sync", strings.Join(names, " or "))
}

func decodedEnvAny(names ...string) (string, bool, error) {
	for _, name := range names {
		if encoded := os.Getenv(name); encoded != "" {
			decoded, err := base64.StdEncoding.DecodeString(encoded)
			if err != nil {
				return "", true, fmt.Errorf("decode %s: %w", name, err)
			}
			return string(decoded), true, nil
		}
	}
	return "", false, nil
}

func buildGcpServiceAccountSecret(ctx context.Context, mc *modal.Client, secretNameEnv string, encodedEnvNames ...string) (*modal.Secret, bool, error) {
	if secretName := os.Getenv(secretNameEnv); secretName != "" {
		secret, err := mc.Secrets.FromName(ctx, secretName, &modal.SecretFromNameParams{
			RequiredKeys: []string{"SERVICE_ACCOUNT_JSON"},
		})
		return secret, true, err
	}
	serviceAccountJSON, ok, err := decodedEnvAny(encodedEnvNames...)
	if err != nil {
		return nil, true, err
	}
	if ok {
		secret, err := mc.Secrets.FromMap(ctx, map[string]string{
			"SERVICE_ACCOUNT_JSON": serviceAccountJSON,
		}, nil)
		return secret, true, err
	}
	return nil, false, nil
}

func buildSecret(ctx context.Context, mc *modal.Client, sync map[string]any) (*modal.Secret, error) {
	if secretName, ok := optionalString(sync, "modal_secret_name"); ok {
		return mc.Secrets.FromName(ctx, secretName, nil)
	}
	if endpoint, ok := optionalString(sync, "endpoint_url"); ok && strings.Contains(endpoint, "storage.googleapis.com") {
		secret, configured, err := buildGcpServiceAccountSecret(
			ctx,
			mc,
			"BUCEPHALUS_MODAL_GCS_SECRET",
			"BUCEPHALUS_MODAL_GCP_SERVICE_ACCOUNT_JSON_B64",
			"BUCEPHALUS_MODAL_GCS_SERVICE_ACCOUNT_JSON_B64",
		)
		if err != nil {
			return nil, err
		}
		if !configured {
			return nil, errors.New("BUCEPHALUS_MODAL_GCP_SERVICE_ACCOUNT_JSON_B64 or BUCEPHALUS_MODAL_GCS_SECRET is required for Modal GCS sync")
		}
		return secret, nil
	}
	accessKey, err := requiredAnyEnv("BUCEPHALUS_MODAL_S3_ACCESS_KEY_ID", "AWS_ACCESS_KEY_ID")
	if err != nil {
		return nil, err
	}
	secretKey, err := requiredAnyEnv("BUCEPHALUS_MODAL_S3_SECRET_ACCESS_KEY", "AWS_SECRET_ACCESS_KEY")
	if err != nil {
		return nil, err
	}
	data := map[string]string{
		"AWS_ACCESS_KEY_ID":     accessKey,
		"AWS_SECRET_ACCESS_KEY": secretKey,
	}
	if token := os.Getenv("BUCEPHALUS_MODAL_S3_SESSION_TOKEN"); token != "" {
		data["AWS_SESSION_TOKEN"] = token
	} else if token := os.Getenv("AWS_SESSION_TOKEN"); token != "" {
		data["AWS_SESSION_TOKEN"] = token
	}
	if region, ok := optionalString(sync, "region"); ok {
		data["AWS_REGION"] = region
	} else if region := os.Getenv("AWS_REGION"); region != "" {
		data["AWS_REGION"] = region
	}
	return mc.Secrets.FromMap(ctx, data, nil)
}

func buildBucketMount(ctx context.Context, mc *modal.Client, sync map[string]any, keyPrefix string, readOnly bool) (*modal.CloudBucketMount, error) {
	if keyPrefix != "" && !strings.HasSuffix(keyPrefix, "/") {
		keyPrefix += "/"
	}
	if boolValue(sync, "force_path_style") {
		return nil, errors.New("BUCEPHALUS_MODAL_S3_FORCE_PATH_STYLE is not supported by Modal's Go SDK CloudBucketMount API")
	}
	secret, err := buildSecret(ctx, mc, sync)
	if err != nil {
		return nil, err
	}
	params := &modal.CloudBucketMountParams{Secret: secret, ReadOnly: readOnly}
	if keyPrefix != "" {
		params.KeyPrefix = &keyPrefix
	}
	if endpoint, ok := optionalString(sync, "endpoint_url"); ok {
		params.BucketEndpointURL = &endpoint
	}
	return mc.CloudBucketMounts.New(stringValue(sync, "bucket"), params)
}

func buildAgentSecret(ctx context.Context, mc *modal.Client, spec map[string]any) (*modal.Secret, error) {
	names := stringList(spec["secret_env"])
	if len(names) == 0 {
		return nil, nil
	}
	data := make(map[string]string, len(names))
	for _, name := range names {
		value, err := requiredEnv(name)
		if err != nil {
			return nil, err
		}
		data[name] = value
	}
	return mc.Secrets.FromMap(ctx, data, nil)
}

func isGcpArtifactRegistryRef(imageRef string) bool {
	registryHost := imageRef
	if slash := strings.Index(registryHost, "/"); slash >= 0 {
		registryHost = registryHost[:slash]
	}
	return strings.HasSuffix(registryHost, ".pkg.dev")
}

func buildGcpArtifactRegistrySecret(ctx context.Context, mc *modal.Client) (*modal.Secret, bool, error) {
	secret, configured, err := buildGcpServiceAccountSecret(
		ctx,
		mc,
		"BUCEPHALUS_MODAL_GCP_ARTIFACT_REGISTRY_SECRET",
		"BUCEPHALUS_MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON_B64",
		"BUCEPHALUS_MODAL_GCP_SERVICE_ACCOUNT_JSON_B64",
	)
	if configured || err != nil {
		return secret, configured, err
	}
	if serviceAccountJSON := os.Getenv("BUCEPHALUS_MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON"); serviceAccountJSON != "" {
		secret, err := mc.Secrets.FromMap(ctx, map[string]string{
			"SERVICE_ACCOUNT_JSON": serviceAccountJSON,
		}, nil)
		return secret, true, err
	}
	return nil, false, nil
}

func imageFromRegistry(ctx context.Context, mc *modal.Client, imageRef string) (*modal.Image, error) {
	if isGcpArtifactRegistryRef(imageRef) {
		secret, configured, err := buildGcpArtifactRegistrySecret(ctx, mc)
		if err != nil {
			return nil, err
		}
		if configured {
			return mc.Images.FromGcpArtifactRegistry(imageRef, secret), nil
		}
	}
	return mc.Images.FromRegistry(imageRef, nil), nil
}

func appLookup(ctx context.Context, mc *modal.Client, appName string, environmentName string) (*modal.App, error) {
	return mc.Apps.FromName(ctx, appName, &modal.AppFromNameParams{
		Environment:     environmentName,
		CreateIfMissing: true,
	})
}

func runtimeWorkersPath(specPath string) string {
	return filepath.Join(filepath.Dir(specPath), "runtime_workers.json")
}

func writeRuntimeWorker(specPath, role string, sandbox *modal.Sandbox) {
	if sandbox == nil || sandbox.SandboxID == "" {
		return
	}
	path := runtimeWorkersPath(specPath)
	payload := map[string]any{"workers": []any{}}
	if data, err := os.ReadFile(path); err == nil {
		_ = json.Unmarshal(data, &payload)
	}
	workers, _ := payload["workers"].([]any)
	for _, item := range workers {
		object := jsonObject(item)
		if stringValue(object, "role") == role && stringValue(object, "sandbox_id") == sandbox.SandboxID {
			return
		}
	}
	workers = append(workers, map[string]any{
		"role":        role,
		"sandbox_id":  sandbox.SandboxID,
		"recorded_at": utcNow(),
	})
	payload["workers"] = workers
	data, _ := json.MarshalIndent(payload, "", "  ")
	_ = os.WriteFile(path, data, 0o644)
}

func makeDir(ctx context.Context, fsys *modal.SandboxFilesystem, remotePath string) error {
	return fsys.MakeDirectory(ctx, remotePath, nil)
}

func copyPath(ctx context.Context, fsys *modal.SandboxFilesystem, localPath, remotePath string) error {
	info, err := os.Stat(localPath)
	if err != nil {
		return err
	}
	if !info.IsDir() {
		parent := path.Dir(remotePath)
		if parent != "." && parent != "/" {
			if err := makeDir(ctx, fsys, parent); err != nil {
				return err
			}
		}
		return fsys.CopyFromLocal(ctx, localPath, remotePath, nil)
	}
	if err := makeDir(ctx, fsys, remotePath); err != nil {
		return err
	}
	root, err := filepath.EvalSymlinks(localPath)
	if err != nil {
		return err
	}
	return filepath.WalkDir(localPath, func(current string, entry os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if current == localPath {
			return nil
		}
		rel, err := filepath.Rel(localPath, current)
		if err != nil {
			return err
		}
		dst := strings.TrimRight(remotePath, "/") + "/" + filepath.ToSlash(rel)
		if entry.Type()&os.ModeSymlink != 0 {
			resolved, err := filepath.EvalSymlinks(current)
			if err != nil {
				return err
			}
			relToRoot, err := filepath.Rel(root, resolved)
			if err != nil || relToRoot == ".." || strings.HasPrefix(relToRoot, "../") {
				return fmt.Errorf("refusing to copy symlink outside directory artifact: %s", current)
			}
			resolvedInfo, err := os.Stat(resolved)
			if err != nil {
				return err
			}
			if resolvedInfo.IsDir() {
				return makeDir(ctx, fsys, dst)
			}
			return fsys.CopyFromLocal(ctx, resolved, dst, nil)
		}
		if entry.IsDir() {
			return makeDir(ctx, fsys, dst)
		}
		return fsys.CopyFromLocal(ctx, current, dst, nil)
	})
}

func fileExists(ctx context.Context, fsys *modal.SandboxFilesystem, remotePath string) bool {
	_, err := fsys.Stat(ctx, remotePath, nil)
	return err == nil
}

func immutableAssetReady(ctx context.Context, fsys *modal.SandboxFilesystem, item map[string]any) bool {
	remotePath := strings.TrimRight(stringValue(item, "remote_path"), "/")
	if boolValue(item, "source_is_dir") {
		return fileExists(ctx, fsys, remotePath+"/.bucephalus_asset_ready")
	}
	return fileExists(ctx, fsys, remotePath)
}

func terminateSandbox(ctx context.Context, sandbox *modal.Sandbox) {
	if sandbox != nil {
		_, _ = sandbox.Terminate(ctx, nil)
	}
}

func stageLaunchMounts(ctx context.Context, mc *modal.Client, app *modal.App, spec map[string]any, writableAssetMount *modal.CloudBucketMount) error {
	items := jsonObjects(spec["launch_mounts"])
	if len(items) == 0 {
		return nil
	}
	image, err := imageFromRegistry(ctx, mc, stringValue(spec, "image"))
	if err != nil {
		return err
	}
	stager, err := mc.Sandboxes.Create(ctx, app, image, &modal.SandboxCreateParams{
		Command:           []string{"sleep", "31536000"},
		CloudBucketMounts: map[string]*modal.CloudBucketMount{"/bucephalus/case_assets": writableAssetMount},
		Timeout:           time.Duration(intValue(spec, "sandbox_timeout_seconds", 3600)) * time.Second,
	})
	if err != nil {
		return err
	}
	defer terminateSandbox(context.Background(), stager)
	fsys := stager.Filesystem()
	for _, item := range items {
		if immutableAssetReady(ctx, fsys, item) {
			continue
		}
		if err := copyPath(ctx, fsys, stringValue(item, "local_path"), stringValue(item, "remote_path")); err != nil {
			return err
		}
		if boolValue(item, "source_is_dir") {
			if err := fsys.WriteText(ctx, "ok\n", strings.TrimRight(stringValue(item, "remote_path"), "/")+"/.bucephalus_asset_ready", nil); err != nil {
				return err
			}
		}
	}
	return nil
}

func createSandbox(ctx context.Context, mc *modal.Client, app *modal.App, imageRef string, caseAssetsMount *modal.CloudBucketMount, spec map[string]any, workdir string, runtimeTransferArchive string) (*modal.Sandbox, error) {
	mounts := map[string]*modal.CloudBucketMount{}
	if caseAssetsMount != nil {
		mounts["/bucephalus/case_assets"] = caseAssetsMount
	}
	secrets := []*modal.Secret{}
	agentSecret, err := buildAgentSecret(ctx, mc, spec)
	if err != nil {
		return nil, err
	}
	if agentSecret != nil {
		secrets = append(secrets, agentSecret)
	}
	params := &modal.SandboxCreateParams{
		Command:           []string{"sleep", "31536000"},
		CloudBucketMounts: mounts,
		Env:               stringMap(spec["env"]),
		Secrets:           secrets,
		BlockNetwork:      boolValue(spec, "block_network"),
		Timeout:           time.Duration(intValue(spec, "sandbox_timeout_seconds", 3600)) * time.Second,
	}
	if cpu := intValue(spec, "cpu_count", 0); cpu > 0 {
		params.CPU = float64(cpu)
	}
	if memory := intValue(spec, "memory_mb", 0); memory > 0 {
		params.MemoryMiB = memory
	}
	image, err := imageFromRegistry(ctx, mc, imageRef)
	if err != nil {
		return nil, err
	}
	sandbox, err := mc.Sandboxes.Create(ctx, app, image, params)
	if err != nil {
		return nil, err
	}
	if runtimeTransferArchive != "" {
		if err := sandbox.Filesystem().CopyFromLocal(ctx, runtimeTransferArchive, runtimeTransferArchivePath, nil); err != nil {
			terminateSandbox(context.Background(), sandbox)
			return nil, err
		}
	}
	return sandbox, nil
}

func bootstrapRuntimeTransferExec(execSpec map[string]any) map[string]any {
	command := stringList(execSpec["command"])
	workdir := stringValue(execSpec, "workdir")
	bootstrapped := cloneObject(execSpec)
	script := "set -e\n" +
		"tar -xzf " + runtimeTransferArchivePath + " -C /\n" +
		"if [ -n \"$1\" ]; then cd \"$1\"; fi\n" +
		"shift\n" +
		"printf 'BUCEPHALUS_AGENT_COMMAND_STARTED_AT=%s\\n' \"$(date -u +%Y-%m-%dT%H:%M:%S.%6NZ)\"\n" +
		"exec \"$@\""
	bootstrapped["command"] = append([]string{"/bin/sh", "-lc", script, "bucephalus-runtime-bootstrap", workdir}, command...)
	delete(bootstrapped, "workdir")
	return bootstrapped
}

func cloneObject(input map[string]any) map[string]any {
	output := make(map[string]any, len(input))
	for key, value := range input {
		output[key] = value
	}
	return output
}

func instrumentContainerStartExec(execSpec map[string]any, markAgentCommandStart bool) map[string]any {
	command := stringList(execSpec["command"])
	workdir := stringValue(execSpec, "workdir")
	instrumented := cloneObject(execSpec)
	script := "printf 'BUCEPHALUS_CONTAINER_STARTED_AT=%s\\n' \"$(date -u +%Y-%m-%dT%H:%M:%S.%6NZ)\"\n" +
		"if [ -n \"$1\" ]; then cd \"$1\"; fi\n" +
		"shift\n"
	if markAgentCommandStart {
		script += "printf 'BUCEPHALUS_AGENT_COMMAND_STARTED_AT=%s\\n' \"$(date -u +%Y-%m-%dT%H:%M:%S.%6NZ)\"\n"
	}
	script += "exec \"$@\""
	instrumented["command"] = append([]string{"/bin/sh", "-lc", script, "bucephalus-container-start", workdir}, command...)
	delete(instrumented, "workdir")
	return instrumented
}

func consumePrefixedLine(text, prefix string) (string, string) {
	rest := strings.TrimPrefix(text, prefix)
	index := strings.IndexByte(rest, '\n')
	if index < 0 {
		return rest, ""
	}
	return rest[:index], rest[index+1:]
}

func splitStartMarkers(stdout string) (*string, *string, string) {
	containerPrefix := "BUCEPHALUS_CONTAINER_STARTED_AT="
	agentPrefix := "BUCEPHALUS_AGENT_COMMAND_STARTED_AT="
	var containerStartedAt *string
	var agentCommandStartedAt *string
	rest := stdout
	for {
		if strings.HasPrefix(rest, containerPrefix) {
			value, tail := consumePrefixedLine(rest, containerPrefix)
			containerStartedAt = &value
			rest = tail
			continue
		}
		if strings.HasPrefix(rest, agentPrefix) {
			value, tail := consumePrefixedLine(rest, agentPrefix)
			agentCommandStartedAt = &value
			rest = tail
			continue
		}
		break
	}
	return containerStartedAt, agentCommandStartedAt, rest
}

func waitAndRead(ctx context.Context, process *modal.ContainerProcess) (int, string, string, error) {
	stdoutCh := make(chan readResult, 1)
	stderrCh := make(chan readResult, 1)
	go func() {
		data, err := io.ReadAll(process.Stdout)
		stdoutCh <- readResult{data: data, err: err}
	}()
	go func() {
		data, err := io.ReadAll(process.Stderr)
		stderrCh <- readResult{data: data, err: err}
	}()
	exitCode, waitErr := process.Wait(ctx)
	stdout := <-stdoutCh
	stderr := <-stderrCh
	if waitErr != nil {
		return exitCode, string(stdout.data), string(stderr.data), waitErr
	}
	if stdout.err != nil {
		return exitCode, string(stdout.data), string(stderr.data), stdout.err
	}
	if stderr.err != nil {
		return exitCode, string(stdout.data), string(stderr.data), stderr.err
	}
	return exitCode, string(stdout.data), string(stderr.data), nil
}

func runProcess(ctx context.Context, sandbox *modal.Sandbox, execSpec map[string]any, result *launchResult, phase string, bootstrapRuntimeTransfer bool) (execRecord, error) {
	if bootstrapRuntimeTransfer {
		execSpec = bootstrapRuntimeTransferExec(execSpec)
	}
	execSpec = instrumentContainerStartExec(execSpec, !bootstrapRuntimeTransfer)
	execStartedAt := utcNow()
	timeout := time.Duration(intValue(execSpec, "timeout_seconds", 300)) * time.Second
	process, err := sandbox.Exec(ctx, stringList(execSpec["command"]), &modal.SandboxExecParams{
		Env:     stringMap(execSpec["env"]),
		Workdir: stringValue(execSpec, "workdir"),
		Timeout: timeout,
	})
	if err != nil {
		return execRecord{}, err
	}
	exitCode, stdout, stderr, err := waitAndRead(ctx, process)
	if err != nil {
		return execRecord{}, err
	}
	containerStartedAt, agentCommandStartedAt, stdout := splitStartMarkers(stdout)
	if output := jsonObject(execSpec["stdout"]); len(output) > 0 {
		localPath := stringValue(output, "local_path")
		if err := os.MkdirAll(filepath.Dir(localPath), 0o755); err != nil {
			return execRecord{}, err
		}
		if err := os.WriteFile(localPath, []byte(stdout), 0o644); err != nil {
			return execRecord{}, err
		}
		if err := sandbox.Filesystem().WriteText(ctx, stdout, stringValue(output, "remote_path"), nil); err != nil {
			return execRecord{}, err
		}
	}
	if output := jsonObject(execSpec["stderr"]); len(output) > 0 {
		localPath := stringValue(output, "local_path")
		if err := os.MkdirAll(filepath.Dir(localPath), 0o755); err != nil {
			return execRecord{}, err
		}
		if err := os.WriteFile(localPath, []byte(stderr), 0o644); err != nil {
			return execRecord{}, err
		}
		if err := sandbox.Filesystem().WriteText(ctx, stderr, stringValue(output, "remote_path"), nil); err != nil {
			return execRecord{}, err
		}
	}
	if phase == "" {
		phase = stringValue(execSpec, "phase")
	}
	record := execRecord{
		Phase:                 phase,
		SandboxID:             sandbox.SandboxID,
		ProcessID:             nil,
		ExitCode:              exitCode,
		TimedOut:              false,
		StartedAt:             execStartedAt,
		ContainerStartedAt:    containerStartedAt,
		AgentCommandStartedAt: agentCommandStartedAt,
		EndedAt:               utcNow(),
	}
	result.Execs = append(result.Execs, record)
	return record, nil
}

func runShellChecked(ctx context.Context, sandbox *modal.Sandbox, label, script, workdir string, timeoutSeconds int) error {
	process, err := sandbox.Exec(ctx, []string{"/bin/sh", "-lc", "set -e\n" + script}, &modal.SandboxExecParams{
		Workdir: workdir,
		Timeout: time.Duration(timeoutSeconds) * time.Second,
	})
	if err != nil {
		return err
	}
	exitCode, stdout, stderr, err := waitAndRead(ctx, process)
	if err != nil {
		return err
	}
	if exitCode != 0 {
		return fmt.Errorf("modal sandbox command %q failed with exit %d\nstdout:\n%s\nstderr:\n%s", label, exitCode, stdout, stderr)
	}
	return nil
}

func runCommandChecked(ctx context.Context, sandbox *modal.Sandbox, label string, command []string, env map[string]string, workdir string, timeoutSeconds int) error {
	process, err := sandbox.Exec(ctx, command, &modal.SandboxExecParams{
		Env:     env,
		Workdir: workdir,
		Timeout: time.Duration(timeoutSeconds) * time.Second,
	})
	if err != nil {
		return err
	}
	exitCode, stdout, stderr, err := waitAndRead(ctx, process)
	if err != nil {
		return err
	}
	if exitCode != 0 {
		return fmt.Errorf("modal sandbox command %q failed with exit %d\nstdout:\n%s\nstderr:\n%s", label, exitCode, stdout, stderr)
	}
	return nil
}

func startSameSandboxEphemeral(ctx context.Context, sandbox *modal.Sandbox, ephemeral map[string]any) error {
	id := stringValue(ephemeral, "id")
	command := stringList(ephemeral["command"])
	if id == "" {
		return errors.New("modal ephemeral missing id")
	}
	if len(command) == 0 {
		return fmt.Errorf("modal ephemeral %q missing command", id)
	}
	stdoutPath := stringValue(jsonObject(ephemeral["stdout"]), "remote_path")
	stderrPath := stringValue(jsonObject(ephemeral["stderr"]), "remote_path")
	pidPath := "/bucephalus/state/ephemerals/" + id + ".pid"
	script := "set -e\n" +
		"workdir=$1\nstdout_path=$2\nstderr_path=$3\npid_path=$4\nshift 4\n" +
		"mkdir -p \"$(dirname \"$stdout_path\")\" \"$(dirname \"$stderr_path\")\" \"$(dirname \"$pid_path\")\"\n" +
		"if [ -n \"$workdir\" ]; then cd \"$workdir\"; fi\n" +
		"(\"$@\" >\"$stdout_path\" 2>\"$stderr_path\" </dev/null & echo $! >\"$pid_path\")"
	startCommand := append([]string{"/bin/sh", "-lc", script, "bucephalus-ephemeral-" + id, stringValue(ephemeral, "workdir"), stdoutPath, stderrPath, pidPath}, command...)
	if err := runCommandChecked(ctx, sandbox, "start_ephemeral_"+id, startCommand, stringMap(ephemeral["env"]), "", 30); err != nil {
		return err
	}
	readiness := jsonObject(ephemeral["readiness"])
	if len(readiness) == 0 {
		return nil
	}
	return runCommandChecked(
		ctx,
		sandbox,
		"readiness_ephemeral_"+id,
		stringList(readiness["command"]),
		stringMap(ephemeral["env"]),
		stringValue(ephemeral, "workdir"),
		intValue(readiness, "timeout_seconds", 30),
	)
}

func startSameSandboxEphemerals(ctx context.Context, sandbox *modal.Sandbox, spec map[string]any) error {
	for _, ephemeral := range jsonObjects(spec["ephemerals"]) {
		if stringValue(ephemeral, "placement") != "same_sandbox" {
			return fmt.Errorf("modal launcher supports only same_sandbox ephemerals, got %q for %q", stringValue(ephemeral, "placement"), stringValue(ephemeral, "id"))
		}
		if err := startSameSandboxEphemeral(ctx, sandbox, ephemeral); err != nil {
			return err
		}
	}
	return nil
}

func copyEphemeralLogsToLocal(ctx context.Context, fsys *modal.SandboxFilesystem, spec map[string]any) {
	for _, ephemeral := range jsonObjects(spec["ephemerals"]) {
		for _, stream := range []string{"stdout", "stderr"} {
			output := jsonObject(ephemeral[stream])
			copyOptionalToLocal(ctx, fsys, stringValue(output, "remote_path"), stringValue(output, "local_path"))
		}
	}
}

func ensureInlineCaptureSize(label, remotePath string, data []byte, maxInlineCaptureBytes *int) error {
	if maxInlineCaptureBytes != nil && len(data) > *maxInlineCaptureBytes {
		return fmt.Errorf("%s capture at %s is too large to inline: bytes=%d max=%d", label, remotePath, len(data), *maxInlineCaptureBytes)
	}
	return nil
}

func selectField(value any, field any) (any, error) {
	fieldText, ok := field.(string)
	if !ok || strings.TrimSpace(fieldText) == "" {
		return value, nil
	}
	fieldText = strings.TrimSpace(fieldText)
	current := value
	if strings.HasPrefix(fieldText, "/") {
		for _, part := range strings.Split(fieldText, "/")[1:] {
			part = strings.ReplaceAll(strings.ReplaceAll(part, "~1", "/"), "~0", "~")
			if list, ok := current.([]any); ok {
				index, err := strconv.Atoi(part)
				if err != nil {
					return nil, err
				}
				current = list[index]
			} else {
				current = jsonObject(current)[part]
			}
		}
		return current, nil
	}
	for _, part := range strings.Split(fieldText, ".") {
		current = jsonObject(current)[part]
	}
	return current, nil
}

func writeLocalCapture(capture map[string]any, data []byte) (*string, error) {
	localPath := stringValue(capture, "local_path")
	if localPath == "" {
		return nil, nil
	}
	if err := os.MkdirAll(filepath.Dir(localPath), 0o755); err != nil {
		return nil, err
	}
	if err := os.WriteFile(localPath, data, 0o644); err != nil {
		return nil, err
	}
	return &localPath, nil
}

func readFileValue(ctx context.Context, fsys *modal.SandboxFilesystem, remotePath, format, label string, maxInlineCaptureBytes *int) (any, error) {
	data, err := fsys.ReadBytes(ctx, remotePath, nil)
	if err != nil {
		return nil, err
	}
	switch format {
	case "json":
		if err := ensureInlineCaptureSize(label, remotePath, data, maxInlineCaptureBytes); err != nil {
			return nil, err
		}
		var value any
		if err := json.Unmarshal(data, &value); err != nil {
			return nil, err
		}
		return value, nil
	case "text":
		if err := ensureInlineCaptureSize(label, remotePath, data, maxInlineCaptureBytes); err != nil {
			return nil, err
		}
		return string(data), nil
	case "bytes":
		return map[string]any{"path": remotePath, "bytes": len(data)}, nil
	default:
		return nil, fmt.Errorf("unsupported runtime output format %q", format)
	}
}

func shellQuote(value string) string {
	if value == "" {
		return "''"
	}
	return "'" + strings.ReplaceAll(value, "'", "'\"'\"'") + "'"
}

func captureOutput(ctx context.Context, sandbox *modal.Sandbox, label string, output map[string]any, workdir string, timeoutSeconds int, maxInlineCaptureBytes *int) (map[string]any, error) {
	fsys := sandbox.Filesystem()
	capture := jsonObject(output["capture"])
	captureType := stringValue(capture, "type")
	switch captureType {
	case "file", "result_json":
		remotePath := stringValue(capture, "path")
		required := boolValue(capture, "required") || captureType == "result_json"
		if !fileExists(ctx, fsys, remotePath) {
			if required {
				return nil, fmt.Errorf("declared runtime output %s missing at %s", label, remotePath)
			}
			return map[string]any{"value": nil, "host_path": nil, "container_path": remotePath, "format": capture["format"]}, nil
		}
		data, err := fsys.ReadBytes(ctx, remotePath, nil)
		if err != nil {
			return nil, err
		}
		hostPath, err := writeLocalCapture(capture, data)
		if err != nil {
			return nil, err
		}
		format := stringValue(capture, "format")
		var value any
		if captureType == "result_json" {
			if err := ensureInlineCaptureSize(label, remotePath, data, maxInlineCaptureBytes); err != nil {
				return nil, err
			}
			var resultJSON any
			if err := json.Unmarshal(data, &resultJSON); err != nil {
				return nil, err
			}
			if _, ok := capture["field"]; ok {
				selected, err := selectField(resultJSON, capture["field"])
				if err != nil {
					return nil, err
				}
				value = map[string]any{"value": selected}
			} else {
				value = resultJSON
			}
			format = "json"
		} else {
			value, err = readFileValue(ctx, fsys, remotePath, format, label, maxInlineCaptureBytes)
			if err != nil {
				return nil, err
			}
		}
		return map[string]any{"value": value, "host_path": hostPath, "container_path": remotePath, "format": format}, nil
	case "workspace_diff":
		patchPath := "/bucephalus/out/candidate.patch"
		probe, err := sandbox.Exec(ctx, []string{"git", "-C", workdir, "rev-parse", "--is-inside-work-tree"}, &modal.SandboxExecParams{Timeout: time.Duration(timeoutSeconds) * time.Second})
		if err != nil {
			return nil, err
		}
		exitCode, _, _, err := waitAndRead(ctx, probe)
		if err != nil {
			return nil, err
		}
		patchText := ""
		if exitCode == 0 {
			pathspec := ". ':(exclude).bucephalus' ':(exclude).haiku' ':(exclude).lab' ':(exclude)logs' ':(exclude)out'"
			if err := runShellChecked(ctx, sandbox, "modal_workspace_diff_add", "git -C "+shellQuote(workdir)+" add -N -- "+pathspec, workdir, timeoutSeconds); err != nil {
				return nil, err
			}
			diff, err := sandbox.Exec(ctx, []string{"/bin/sh", "-lc", "git -C " + shellQuote(workdir) + " diff --binary -- " + pathspec}, &modal.SandboxExecParams{Workdir: workdir, Timeout: time.Duration(timeoutSeconds) * time.Second})
			if err != nil {
				return nil, err
			}
			diffExit, stdout, _, err := waitAndRead(ctx, diff)
			if err != nil {
				return nil, err
			}
			if diffExit != 0 {
				return nil, errors.New("failed to capture modal workspace diff")
			}
			patchText = stdout
			if maxInlineCaptureBytes != nil && len([]byte(patchText)) > *maxInlineCaptureBytes {
				return nil, fmt.Errorf("%s workspace_diff is too large to inline: bytes=%d max=%d", label, len([]byte(patchText)), *maxInlineCaptureBytes)
			}
		}
		if err := fsys.WriteText(ctx, patchText, patchPath, nil); err != nil {
			return nil, err
		}
		hostPath, err := writeLocalCapture(capture, []byte(patchText))
		if err != nil {
			return nil, err
		}
		return map[string]any{"value": map[string]any{"patch": patchText, "path": patchPath}, "host_path": hostPath, "container_path": patchPath, "format": "unified_diff"}, nil
	default:
		return nil, fmt.Errorf("%s.capture.type %q is not executable", label, captureType)
	}
}

func captureOutputs(ctx context.Context, sandbox *modal.Sandbox, outputs map[string]map[string]any, prefix, workdir string, timeoutSeconds int, maxInlineCaptureBytes *int) (map[string]any, error) {
	captured := make(map[string]any, len(outputs))
	keys := make([]string, 0, len(outputs))
	for key := range outputs {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	for _, outputID := range keys {
		value, err := captureOutput(ctx, sandbox, prefix+"."+outputID, outputs[outputID], workdir, timeoutSeconds, maxInlineCaptureBytes)
		if err != nil {
			return nil, err
		}
		captured[outputID] = value
	}
	return captured, nil
}

func selectTransportSource(source map[string]any, agentOutputs map[string]any, taskPayload any) (any, error) {
	if output, ok := optionalString(source, "output"); ok {
		outputID := strings.TrimPrefix(output, "agent.")
		outputValue := jsonObject(agentOutputs[outputID])["value"]
		if _, ok := source["field"]; ok {
			return selectField(outputValue, source["field"])
		}
		return outputValue, nil
	}
	if _, ok := source["case"]; ok {
		return selectField(taskPayload, source["case"])
	}
	if _, ok := source["task"]; ok {
		return selectField(taskPayload, source["task"])
	}
	if object := jsonObject(source["object"]); len(object) > 0 {
		out := make(map[string]any, len(object))
		for key, nested := range object {
			value, err := selectTransportSource(jsonObject(nested), agentOutputs, taskPayload)
			if err != nil {
				return nil, err
			}
			out[key] = value
		}
		return out, nil
	}
	return nil, nil
}

func valueToBytes(value any, jsonMode bool) ([]byte, error) {
	if !jsonMode {
		if text, ok := value.(string); ok {
			return []byte(text), nil
		}
	}
	return json.MarshalIndent(value, "", "  ")
}

func materializeGraderInputs(ctx context.Context, sandbox *modal.Sandbox, grader map[string]any, agentOutputs map[string]any, taskPayload any) (map[string]string, error) {
	env := map[string]string{}
	fsys := sandbox.Filesystem()
	for inputID, inputSpec := range jsonObjectMap(grader["inputs"]) {
		value, err := selectTransportSource(jsonObject(inputSpec["source"]), agentOutputs, taskPayload)
		if err != nil {
			return nil, err
		}
		if value == nil {
			if boolValue(inputSpec, "required") {
				return nil, fmt.Errorf("required grader input %q resolved to null", inputID)
			}
			continue
		}
		materialize := jsonObject(inputSpec["materialize"])
		switch stringValue(materialize, "as") {
		case "file", "json_file":
			remotePath := stringValue(materialize, "path")
			data, err := valueToBytes(value, stringValue(materialize, "as") == "json_file")
			if err != nil {
				return nil, err
			}
			parent := path.Dir(remotePath)
			if parent != "." && parent != "/" {
				if err := makeDir(ctx, fsys, parent); err != nil {
					return nil, err
				}
			}
			if err := fsys.WriteBytes(ctx, data, remotePath, nil); err != nil {
				return nil, err
			}
		case "env":
			if text, ok := value.(string); ok {
				env[stringValue(materialize, "name")] = text
			} else {
				encoded, _ := json.Marshal(value)
				env[stringValue(materialize, "name")] = string(encoded)
			}
		default:
			return nil, fmt.Errorf("grader input %q.materialize.as %q is not executable", inputID, stringValue(materialize, "as"))
		}
	}
	return env, nil
}

func writeTransportEnvelope(ctx context.Context, fsys *modal.SandboxFilesystem, spec map[string]any, agentOutputs, graderOutputs map[string]any) error {
	envelope := map[string]any{
		"schema_version": "runtime_transport_envelope_v1",
		"agent":          map[string]any{"outputs": agentOutputs},
		"grader":         map[string]any{"outputs": graderOutputs},
	}
	payload, err := json.MarshalIndent(envelope, "", "  ")
	if err != nil {
		return err
	}
	transport := jsonObject(spec["transport_envelope"])
	if err := fsys.WriteBytes(ctx, payload, stringValue(transport, "remote_path"), nil); err != nil {
		return err
	}
	localPath := stringValue(transport, "local_path")
	if err := os.MkdirAll(filepath.Dir(localPath), 0o755); err != nil {
		return err
	}
	return os.WriteFile(localPath, payload, 0o644)
}

func prepareModalGrader(ctx context.Context, taskSandbox *modal.Sandbox, grader map[string]any) error {
	timeoutSeconds := intValue(grader, "timeout_seconds", 300)
	for _, binding := range jsonObjects(grader["hidden_assets"]) {
		stashParent := path.Dir(stringValue(binding, "stash_path"))
		script := "mkdir -p " + shellQuote(stashParent) + "\nrm -rf " + shellQuote(stringValue(binding, "stash_path")) + "\nmv " + shellQuote(stringValue(binding, "hidden_path")) + " " + shellQuote(stringValue(binding, "stash_path"))
		if err := runShellChecked(ctx, taskSandbox, "hide_hidden_asset", script, "", timeoutSeconds); err != nil {
			return err
		}
	}
	return nil
}

func revealModalGraderAssets(ctx context.Context, taskSandbox *modal.Sandbox, grader map[string]any) error {
	timeoutSeconds := intValue(grader, "timeout_seconds", 300)
	for _, binding := range jsonObjects(grader["hidden_assets"]) {
		parent := path.Dir(stringValue(binding, "revealed_path"))
		script := "mkdir -p " + shellQuote(parent) + "\nrm -rf " + shellQuote(stringValue(binding, "revealed_path")) + "\nmv " + shellQuote(stringValue(binding, "stash_path")) + " " + shellQuote(stringValue(binding, "revealed_path"))
		if err := runShellChecked(ctx, taskSandbox, "reveal_hidden_asset", script, "", timeoutSeconds); err != nil {
			return err
		}
	}
	injected := jsonObject(grader["injected"])
	if len(injected) == 0 {
		return nil
	}
	src := stringValue(injected, "source_remote_path")
	dest := stringValue(injected, "copy_dest")
	var extract string
	if boolValue(injected, "source_is_dir") {
		extract = "cp -R " + shellQuote(src) + "/. " + shellQuote(dest)
	} else if archiveFlag, ok := optionalString(injected, "archive_flag"); ok {
		extract = "tar " + shellQuote(archiveFlag) + " " + shellQuote(src) + " -C " + shellQuote(dest)
	} else {
		extract = "cp " + shellQuote(src) + " " + shellQuote(dest) + "/"
	}
	return runShellChecked(ctx, taskSandbox, "injected_grader_bundle", "mkdir -p "+shellQuote(dest)+"\nfind "+shellQuote(dest)+" -mindepth 1 -maxdepth 1 -exec rm -rf {} +\n"+extract, "", timeoutSeconds)
}

func copyOptionalToLocal(ctx context.Context, fsys *modal.SandboxFilesystem, remotePath, localPath string) bool {
	if remotePath == "" || localPath == "" {
		return false
	}
	if err := os.MkdirAll(filepath.Dir(localPath), 0o755); err != nil {
		return false
	}
	return fsys.CopyToLocal(ctx, remotePath, localPath, nil) == nil
}

func exportLocalFileToBucket(ctx context.Context, mc *modal.Client, app *modal.App, spec map[string]any, sync map[string]any, localPath, remotePath string) (bool, error) {
	if _, err := os.Stat(localPath); err != nil {
		if os.IsNotExist(err) {
			return false, nil
		}
		return false, err
	}
	writableMount, err := buildBucketMount(ctx, mc, sync, stringValue(sync, "prefix"), false)
	if err != nil {
		return false, err
	}
	image, err := imageFromRegistry(ctx, mc, stringValue(spec, "image"))
	if err != nil {
		return false, err
	}
	stager, err := mc.Sandboxes.Create(ctx, app, image, &modal.SandboxCreateParams{
		Command:           []string{"sleep", "31536000"},
		CloudBucketMounts: map[string]*modal.CloudBucketMount{"/bucephalus": writableMount},
		Timeout:           time.Duration(intValue(spec, "sandbox_timeout_seconds", 3600)) * time.Second,
	})
	if err != nil {
		return false, err
	}
	defer terminateSandbox(context.Background(), stager)
	if err := copyPath(ctx, stager.Filesystem(), localPath, remotePath); err != nil {
		return false, err
	}
	return true, nil
}

func launcherErrorStderrPath(specPath string, spec map[string]any) string {
	execs := jsonObjects(spec["execs"])
	if len(execs) > 0 {
		if stderr := jsonObject(execs[len(execs)-1]["stderr"]); len(stderr) > 0 {
			if localPath := stringValue(stderr, "local_path"); localPath != "" {
				return localPath
			}
		}
	}
	return filepath.Join(filepath.Dir(specPath), "modal_launcher_stderr.log")
}

func appendLauncherError(specPath string, spec map[string]any, err error) {
	stderrPath := launcherErrorStderrPath(specPath, spec)
	_ = os.MkdirAll(filepath.Dir(stderrPath), 0o755)
	file, openErr := os.OpenFile(stderrPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if openErr != nil {
		return
	}
	defer file.Close()
	fmt.Fprintf(file, "\n[bucephalus modal launcher error]\n%v\n", err)
}

func isTimeoutError(err error) bool {
	if err == nil {
		return false
	}
	text := strings.ToLower(err.Error())
	return strings.Contains(text, "timeout") || strings.Contains(text, "timed out") || errors.Is(err, context.DeadlineExceeded)
}

func runLaunch(specPath string) error {
	ctx := context.Background()
	timings := map[string]string{}
	timingMark(timings, "launcher_main_started_at")
	spec, err := loadJSON(specPath)
	if err != nil {
		return err
	}
	var maxInlineCaptureBytes *int
	if raw, ok := spec["max_inline_capture_bytes"]; ok && raw != nil {
		value := intValue(map[string]any{"v": raw}, "v", 0)
		maxInlineCaptureBytes = &value
	}
	runtimeTransferArchive := stringValue(spec, "runtime_transfer_archive")
	archiveInfo, err := os.Stat(runtimeTransferArchive)
	if err != nil {
		return err
	}
	sync := jsonObject(spec["sync"])
	timingMark(timings, "app_lookup_started_at")
	mc, err := modal.NewClient()
	if err != nil {
		return err
	}
	app, err := appLookup(ctx, mc, stringValue(spec, "app_name"), stringValue(spec, "environment_name"))
	if err != nil {
		return err
	}
	timingMark(timings, "app_lookup_ended_at")
	timingMark(timings, "runtime_transfer_archive_build_started_at")
	timingMark(timings, "runtime_transfer_archive_build_ended_at")
	var caseAssetsMount *modal.CloudBucketMount
	timingMark(timings, "launch_mounts_prepare_started_at")
	if len(jsonObjects(spec["launch_mounts"])) > 0 {
		writableAssetMount, err := buildBucketMount(ctx, mc, sync, stringValue(sync, "immutable_case_asset_prefix"), false)
		if err != nil {
			return err
		}
		if err := stageLaunchMounts(ctx, mc, app, spec, writableAssetMount); err != nil {
			return err
		}
		caseAssetsMount, err = buildBucketMount(ctx, mc, sync, stringValue(sync, "immutable_case_asset_prefix"), true)
		if err != nil {
			return err
		}
	}
	timingMark(timings, "launch_mounts_prepare_ended_at")
	var sandbox *modal.Sandbox
	var graderSandbox *modal.Sandbox
	startedAt := utcNow()
	var endedAt *string
	var exitCode *int
	result := &launchResult{
		SandboxID:                   nil,
		Execs:                       []execRecord{},
		ExitCode:                    nil,
		TimedOut:                    false,
		StartedAt:                   startedAt,
		EndedAt:                     nil,
		RuntimeTransferArchiveBytes: archiveInfo.Size(),
		Timings:                     timings,
	}
	var fatalErr error
	runErr := func() error {
		timingMark(timings, "sandbox_create_started_at")
		var err error
		sandbox, err = createSandbox(ctx, mc, app, stringValue(spec, "image"), caseAssetsMount, spec, stringValue(spec, "workdir"), runtimeTransferArchive)
		if err != nil {
			return err
		}
		timingMark(timings, "sandbox_create_ended_at")
		result.SandboxID = &sandbox.SandboxID
		writeRuntimeWorker(specPath, "task", sandbox)
		fsys := sandbox.Filesystem()
		grader := jsonObject(spec["grader"])
		ephemerals := jsonObjects(spec["ephemerals"])
		bootstrapRuntimeTransfer := len(grader) == 0 && len(ephemerals) == 0
		if !bootstrapRuntimeTransfer {
			if err := runShellChecked(ctx, sandbox, "runtime_transfer_extract", "tar -xzf "+runtimeTransferArchivePath+" -C /", "", intValue(spec, "sandbox_timeout_seconds", 3600)); err != nil {
				return err
			}
			if err := prepareModalGrader(ctx, sandbox, grader); err != nil {
				return err
			}
		}
		if err := startSameSandboxEphemerals(ctx, sandbox, spec); err != nil {
			return err
		}
		for index, execSpec := range jsonObjects(spec["execs"]) {
			record, err := runProcess(ctx, sandbox, execSpec, result, "", bootstrapRuntimeTransfer && index == 0)
			if err != nil {
				return err
			}
			exitCode = &record.ExitCode
		}
		if len(grader) == 0 {
			return nil
		}
		trialInputBytes, err := fsys.ReadBytes(ctx, stringValue(jsonObject(spec["trial_input"]), "remote_path"), nil)
		if err != nil {
			return err
		}
		var taskPayload any
		if err := json.Unmarshal(trialInputBytes, &taskPayload); err != nil {
			return err
		}
		agentOutputs, err := captureOutputs(ctx, sandbox, jsonObjectMap(grader["agent_outputs"]), "agent", stringValue(spec, "workdir"), intValue(grader, "timeout_seconds", 300), maxInlineCaptureBytes)
		if err != nil {
			return err
		}
		if err := revealModalGraderAssets(ctx, sandbox, grader); err != nil {
			return err
		}
		graderSandbox = sandbox
		if stringValue(grader, "sandbox") == "separate" {
			graderSandbox, err = createSandbox(ctx, mc, app, stringValue(grader, "image"), caseAssetsMount, spec, stringValue(grader, "workdir"), runtimeTransferArchive)
			if err != nil {
				return err
			}
			writeRuntimeWorker(specPath, "grading", graderSandbox)
		}
		transportEnv, err := materializeGraderInputs(ctx, graderSandbox, grader, agentOutputs, taskPayload)
		if err != nil {
			return err
		}
		graderEnv := stringMap(grader["env"])
		agentStatus := "signal"
		if result.TimedOut {
			agentStatus = "timeout"
		} else if exitCode != nil {
			agentStatus = strconv.Itoa(*exitCode)
		}
		for key, value := range graderEnv {
			if value == "__BUCEPHALUS_AGENT_EXIT_STATUS__" {
				graderEnv[key] = agentStatus
			}
		}
		for key, value := range transportEnv {
			graderEnv[key] = value
		}
		graderExec := map[string]any{
			"phase":           "grader",
			"command":         stringList(grader["command"]),
			"env":             graderEnv,
			"workdir":         stringValue(grader, "workdir"),
			"timeout_seconds": intValue(grader, "timeout_seconds", 300),
			"stdout":          grader["stdout"],
			"stderr":          grader["stderr"],
		}
		if _, err := runProcess(ctx, graderSandbox, graderExec, result, "grader", false); err != nil {
			return err
		}
		graderOutputs, err := captureOutputs(ctx, graderSandbox, jsonObjectMap(grader["outputs"]), "grader", stringValue(grader, "workdir"), intValue(grader, "timeout_seconds", 300), maxInlineCaptureBytes)
		if err != nil {
			return err
		}
		return writeTransportEnvelope(ctx, graderSandbox.Filesystem(), spec, agentOutputs, graderOutputs)
	}()
	if runErr != nil {
		result.TimedOut = isTimeoutError(runErr)
		appendLauncherError(specPath, spec, runErr)
		if !result.TimedOut {
			errText := runErr.Error()
			result.LauncherError = &errText
			fatalErr = runErr
		}
	}
	now := utcNow()
	endedAt = &now
	if sandbox != nil {
		timingMark(timings, "result_copy_started_at")
		fsys := sandbox.Filesystem()
		copyOptionalToLocal(ctx, fsys, stringValue(jsonObject(spec["result"]), "remote_path"), stringValue(jsonObject(spec["result"]), "local_path"))
		copyOptionalToLocal(ctx, fsys, stringValue(jsonObject(spec["events"]), "scratch_path"), stringValue(jsonObject(spec["events"]), "local_path"))
		copyEphemeralLogsToLocal(ctx, fsys, spec)
		transportFS := fsys
		if graderSandbox != nil {
			transportFS = graderSandbox.Filesystem()
		}
		copyOptionalToLocal(ctx, transportFS, stringValue(jsonObject(spec["transport_envelope"]), "remote_path"), stringValue(jsonObject(spec["transport_envelope"]), "local_path"))
		if grader := jsonObject(spec["grader"]); len(grader) > 0 {
			copyOptionalToLocal(ctx, transportFS, stringValue(jsonObject(grader["stdout"]), "remote_path"), stringValue(jsonObject(grader["stdout"]), "local_path"))
			copyOptionalToLocal(ctx, transportFS, stringValue(jsonObject(grader["stderr"]), "remote_path"), stringValue(jsonObject(grader["stderr"]), "local_path"))
		}
		timingMark(timings, "result_copy_ended_at")
	}
	result.ExitCode = exitCode
	result.EndedAt = endedAt
	timingMark(timings, "result_available_at")
	marker("BUCEPHALUS_MODAL_RESULT", result)
	if sandbox != nil {
		if durableEventsPath, ok := optionalString(jsonObject(spec["events"]), "durable_path"); ok {
			localEventsPath := stringValue(jsonObject(spec["events"]), "local_path")
			timingMark(timings, "durable_events_export_started_at")
			_, _ = exportLocalFileToBucket(ctx, mc, app, spec, sync, localEventsPath, durableEventsPath)
			timingMark(timings, "durable_events_export_ended_at")
		}
	}
	timingMark(timings, "sandbox_cleanup_started_at")
	if graderSandbox != nil && sandbox != nil && graderSandbox.SandboxID != sandbox.SandboxID {
		terminateSandbox(context.Background(), graderSandbox)
	}
	terminateSandbox(context.Background(), sandbox)
	timingMark(timings, "sandbox_cleanup_ended_at")
	timingMark(timings, "launcher_completed_at")
	marker("BUCEPHALUS_MODAL_LIFECYCLE", map[string]any{"sandbox_id": result.SandboxID, "timings": timings})
	return fatalErr
}

func isNotFound(err error) bool {
	if err == nil {
		return false
	}
	text := strings.ToLower(err.Error())
	return strings.Contains(text, "notfound") || strings.Contains(text, "not found") || strings.Contains(text, "404")
}

func runCleanup(specPath string) error {
	ctx := context.Background()
	spec, err := loadJSON(specPath)
	if err != nil {
		return err
	}
	mc, err := modal.NewClient()
	if err != nil {
		return err
	}
	results := []map[string]any{}
	errorsOut := []map[string]any{}
	cleaned := 0
	for _, sandboxID := range stringList(spec["sandbox_ids"]) {
		sandbox, err := mc.Sandboxes.FromID(ctx, sandboxID)
		if err != nil {
			if isNotFound(err) {
				cleaned++
				results = append(results, map[string]any{"sandbox_id": sandboxID, "status": "not_found"})
			} else {
				errorsOut = append(errorsOut, map[string]any{"sandbox_id": sandboxID, "error": err.Error()})
			}
			continue
		}
		if _, err := sandbox.Terminate(ctx, nil); err != nil {
			if isNotFound(err) {
				cleaned++
				results = append(results, map[string]any{"sandbox_id": sandboxID, "status": "not_found"})
			} else {
				errorsOut = append(errorsOut, map[string]any{"sandbox_id": sandboxID, "error": err.Error()})
			}
			continue
		}
		cleaned++
		results = append(results, map[string]any{"sandbox_id": sandboxID, "status": "terminated"})
	}
	payload := map[string]any{"cleaned": cleaned, "results": results, "errors": errorsOut}
	marker("BUCEPHALUS_MODAL_CLEANUP", payload)
	if len(errorsOut) > 0 {
		return errors.New("modal cleanup failed")
	}
	return nil
}

func main() {
	if len(os.Args) != 3 {
		fail("usage: %s launch|cleanup SPEC.json", os.Args[0])
	}
	var err error
	switch os.Args[1] {
	case "launch":
		err = runLaunch(os.Args[2])
	case "cleanup":
		err = runCleanup(os.Args[2])
	default:
		err = fmt.Errorf("unknown mode %q", os.Args[1])
	}
	if err != nil {
		fail("%v", err)
	}
}
