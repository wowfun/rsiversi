Read the family [contract](README.md) before changing retention or capture.

- Keep cache failure independent of Tool success and process progress. Process
  producers retain complete bytes before tail truncation; the policy only changes
  model-facing text and preserves canonical values.
- Validate locators and page bounds at decoded boundaries. Local storage owns
  private paths, writer leases, bounded admission, publication and cleanup;
  consumers never interpret a locator as filesystem authority.
- Keep Session scopes and product defaults in their owners. Spill does not depend
  on Session, Media or the standard Host, and it does not promise durable retention.
- Exercise raw-byte reconstruction, failure fallback and capture/cleanup races
  through the owning core, local, policy or API tests. Real subprocess capture
  belongs to the Process integration tests.
