use super::SessionController;
use crate::{
    ObservationFailure, ObservationKind,
    observation::{Retry, delivered_cursor},
};
use futures_util::{StreamExt, future::BoxFuture};
use rsi_agent_turn_protocol::SessionObservation;
use rsi_session_protocol::{SessionError, SurfaceSource, SurfaceUpdate, SurfaceView};
use std::sync::Arc;

fn sink_kind(source: SurfaceSource) -> Option<ObservationKind> {
    match source {
        SurfaceSource::History => Some(ObservationKind::Facts),
        SurfaceSource::Model => Some(ObservationKind::Models),
        SurfaceSource::Interactions => Some(ObservationKind::Interactions),
        SurfaceSource::Projections => Some(ObservationKind::Projections),
        SurfaceSource::Goal => None,
    }
}

impl SessionController {
    pub(super) fn follow_surface(
        self: &Arc<Self>,
        mut view: SurfaceView,
        mut failure: Option<(ObservationKind, ObservationFailure)>,
    ) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let mut retry = Retry::default();
            loop {
                if let Some((kind, error)) = failure.take() {
                    if let Err(error) = retry
                        .after(error, kind, self.sink.as_ref(), &self.execution)
                        .await
                    {
                        if view.includes(SurfaceSource::Goal) {
                            self.goal
                                .send_replace(Some(Err(SessionError::Backend(error.to_string()))));
                        }
                        self.sink.stopped(kind, &error).await;
                        return;
                    }
                }
                let result = self.receive_surface(&mut view, &mut retry).await;
                match result {
                    Ok(()) => return,
                    Err(reason) => failure = Some(reason),
                }
            }
        })
    }

    fn goal_failure(self: &Arc<Self>, error: SessionError) {
        if !matches!(
            error,
            SessionError::Capacity | SessionError::Api(rsi_api_protocol::ApiError::Capacity)
        ) {
            self.goal.send_replace(Some(Err(error)));
            return;
        }
        let controller = self.clone();
        self.execution.spawn(self.tasks.track_future(async move {
            let observe = async {
                let mut stream =
                    crate::read::resume_capacity_read(Err(error), &controller.execution, || {
                        controller.handle.observe_goal()
                    })
                    .await?;
                while let Some(value) = stream.next().await {
                    controller.publish_goal(value?);
                }
                Err::<(), _>(SessionError::Backend("Goal observation ended".into()))
            };
            tokio::select! {
                biased;
                () = controller.stop.cancelled() => {},
                result = observe => if let Err(error) = result {
                    controller.goal.send_replace(Some(Err(error)));
                },
            }
        }));
    }

    async fn receive_surface(
        self: &Arc<Self>,
        view: &mut SurfaceView,
        retry: &mut Retry,
    ) -> Result<(), (ObservationKind, ObservationFailure)> {
        // A shared transport failure is reported against the remaining feed.
        let kind = |view: SurfaceView| {
            [
                SurfaceSource::History,
                SurfaceSource::Projections,
                SurfaceSource::Model,
                SurfaceSource::Interactions,
            ]
            .into_iter()
            .find(|source| view.includes(*source))
            .and_then(sink_kind)
        };
        let transport_failure = |view, error: SessionError| {
            if let Some(kind) = kind(view) {
                Err((kind, error.into()))
            } else {
                self.goal.send_replace(Some(Err(error)));
                Ok(())
            }
        };
        let mut stream = match self.handle.clone().observe_surface(*view).await {
            Ok(stream) => stream,
            Err(error) => return transport_failure(*view, error),
        };
        while let Some(item) = stream.next().await {
            let item = match item {
                Ok(item) => item,
                Err(error) => return transport_failure(*view, error),
            };
            let result = match item {
                SurfaceUpdate::Stopped(source, error) => {
                    let remaining = view.without(source);
                    if let Some(kind) = sink_kind(source) {
                        if remaining.is_none() {
                            return Err((kind, error.into()));
                        }
                        self.spawn_feed(view.one(source), Some((kind, error.into())));
                    } else {
                        self.goal_failure(error);
                    }
                    if let Some(remaining) = remaining {
                        *view = remaining;
                        continue;
                    }
                    return Ok(());
                }
                SurfaceUpdate::History(record) => {
                    let cursor = delivered_cursor(view.cursor(), &record);
                    let workflow = matches!(&record, SessionObservation::Control { record, .. }
                        if matches!(record.body(), rsi_agent_session_protocol::AgentControlRecordBody::ProgramRun { .. }));
                    self.sink
                        .observation(record)
                        .await
                        .map_err(|error| (ObservationKind::Facts, error))?;
                    *view = view.resume(cursor);
                    if workflow {
                        self.workflow_changed
                            .send_modify(|rev| *rev = rev.wrapping_add(1));
                    }
                    Ok(())
                }
                SurfaceUpdate::Model(model) => self
                    .sink
                    .model(model)
                    .await
                    .map_err(|e| (ObservationKind::Models, e)),
                SurfaceUpdate::Interactions(value) => self
                    .sink
                    .interactions(value)
                    .await
                    .map_err(|e| (ObservationKind::Interactions, e)),
                SurfaceUpdate::Projections(snapshot) => {
                    self.sink
                        .projections(snapshot.clone())
                        .await
                        .map_err(|e| (ObservationKind::Projections, e))?;
                    self.projections.send_replace(Some(snapshot));
                    Ok(())
                }
                SurfaceUpdate::Goal(value) => {
                    self.publish_goal(value);
                    Ok(())
                }
            };
            result?;
            *retry = Retry::default();
        }
        if let Some(kind) = kind(*view) {
            Err((kind, ObservationFailure::Ended))
        } else {
            self.goal.send_replace(Some(Err(SessionError::Backend(
                "Goal observation ended".into(),
            ))));
            Ok(())
        }
    }
}
