# rsi-client-composition

This stateless product library declares the standard independent Session,
Workspace, Models, Output, Settings, Media and Session Files client factories and their Local
markers. Native UDS, native HTTP and browser application composition use the same
ordered domain entries. Callers add their transport and application plugins and
select the Profile lifetime; this library activates nothing and creates no
Runtime. Domains still own their API descriptors and proxy implementations.
