# Runner Image Contract

Production runner VMs should be created from a versioned image, not hand
bootstrapped after the fact. This directory records the image contract that a
provider provisioner must satisfy before the VM can join a runner pool.

`runner-image.manifest.json` is the current contract. A real image bake should:

- consume a `scripts/release/build-buc-release.sh` release bundle
- install the Bucephalus Core binary at `/usr/local/bin/bucephalus`
- install Bun somewhere executable by the service user, such as `/usr/local/bin`
- install the Cloud worker source at `/opt/bucephalus-cloud`
- install `deploy/runner-vm/bucephalus-runner.service`
- provide Docker only for images that advertise `docker_daemon`
- leave credentials out of the image and inject them at provision time

The provisioner owns the provider-specific work: choosing VM size, disk size,
network, IAM/service account, image id, and cloud-init or secrets-manager
injection. The worker owns the runtime proof: registering with Cloud,
heartbeating, validating resources, claiming runs, invoking Core, and poisoning
the VM on cleanup failure.

Hand-running `deploy/runner-vm/bootstrap-runner-vm.sh` is still useful for local
VM development, but it is not the production lifecycle boundary.
