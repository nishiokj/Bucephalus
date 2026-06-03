output "artifact_repository" {
  description = "Artifact Registry repository for digest-addressed control-plane images."
  value = {
    name     = google_artifact_registry_repository.cloud.name
    location = google_artifact_registry_repository.cloud.location
  }
}

output "control_plane_services" {
  description = "Cloud Run service names and URIs."
  value = {
    deployed = var.deploy_control_plane_services
    api = var.deploy_control_plane_services ? {
      name = google_cloud_run_v2_service.api[0].name
      uri  = google_cloud_run_v2_service.api[0].uri
    } : null
    pool_controller = var.deploy_control_plane_services ? {
      name = google_cloud_run_v2_service.pool_controller[0].name
      uri  = google_cloud_run_v2_service.pool_controller[0].uri
    } : null
    migrations_job = var.deploy_control_plane_services ? {
      name = google_cloud_run_v2_job.migrations[0].name
    } : null
  }
}

output "database" {
  description = "Private Cloud SQL database identity. Connection material is stored only in Secret Manager versions created outside Terraform."
  value = {
    instance_connection_name = google_sql_database_instance.primary.connection_name
    private_ip_address       = google_sql_database_instance.primary.private_ip_address
    database_name            = google_sql_database.cloud.name
  }
}

output "service_accounts" {
  description = "Service identities with scoped runtime and migration duties."
  value = {
    api             = google_service_account.api.email
    pool_controller = google_service_account.pool_controller.email
    migrator        = google_service_account.migrator.email
  }
}

output "user_oauth" {
  description = "User OAuth verifier settings injected into the API. The client ID is not a secret."
  value = {
    issuer    = var.oauth_issuer
    client_id = var.oauth_user_client_id
    jwks_url  = var.oauth_jwks_url
  }
}

output "secret_names" {
  description = "Secret Manager secret names. Terraform creates containers and IAM bindings only, not secret versions."
  value       = local.secret_ids
}

output "network" {
  description = "Private network resources owned by this substrate."
  value = {
    vpc        = google_compute_network.control_plane.name
    subnet     = google_compute_subnetwork.control_plane.name
    connector  = google_vpc_access_connector.control_plane.name
    db_private = true
  }
}
