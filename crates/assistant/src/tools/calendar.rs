//! Calendar tools: read, find slots, create/update/delete items, schedule moves.

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{AssistantError, Result};
use crate::roles::Role;
use super::{parse_date, parse_str, parse_time_opt, parse_uuid, pending_confirmation, Safety, Tool, ToolCtx};

// ── GetCalendar ───────────────────────────────────────────────────────────────

pub struct GetCalendar;

#[async_trait]
impl Tool for GetCalendar {
    fn name(&self) -> &'static str { "get_calendar" }
    fn description(&self) -> &'static str {
        "Gibt Kalendereinträge für einen Datumsbereich zurück (Umzugstermine, Mitarbeiteraufgaben, interne Ereignisse). Jeder Eintrag hat ein Feld 'kind': \"termin\" = Kalendereintrag (ID ist calendar_item), \"auftrag\" = Umzug aus einer Anfrage (ID ist inquiry_id). Danach richtet sich, welches Schreib-Tool zu verwenden ist."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "format": "date" },
                "to":   { "type": "string", "format": "date" }
            },
            "required": ["from", "to"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let from = parse_date(args, "from", self.name())?;
        let to = parse_date(args, "to", self.name())?;
        let items = ctx.services.calendar.get_range(from, to).await?;
        let count = items.len();
        Ok(json!({ "items": items, "count": count }))
    }
}

// ── FindAvailableSlots ────────────────────────────────────────────────────────

pub struct FindAvailableSlots;

#[async_trait]
impl Tool for FindAvailableSlots {
    fn name(&self) -> &'static str { "find_available_slots" }
    fn description(&self) -> &'static str {
        "Sucht Tage mit freier Crew-Kapazität im angegebenen Zeitraum."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "volume_m3":   { "type": "number", "minimum": 0 },
                "distance_km": { "type": "number", "minimum": 0 },
                "earliest":    { "type": "string", "format": "date" },
                "latest":      { "type": "string", "format": "date" }
            },
            "required": ["earliest", "latest"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let earliest = parse_date(args, "earliest", self.name())?;
        let latest = parse_date(args, "latest", self.name())?;
        let slots = ctx.services.calendar.find_available_slots(earliest, latest).await?;
        let count = slots.len();
        Ok(json!({ "slots": slots, "count": count }))
    }
}

// ── GetEmployeeAssignments ────────────────────────────────────────────────────

pub struct GetEmployeeAssignments;

#[async_trait]
impl Tool for GetEmployeeAssignments {
    fn name(&self) -> &'static str { "get_employee_assignments" }
    fn description(&self) -> &'static str {
        "Listet Einsätze (Umzüge + Kalendereinträge) eines Mitarbeiters im Zeitraum."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "employee_id": { "type": "string", "format": "uuid" },
                "from":        { "type": "string", "format": "date" },
                "to":          { "type": "string", "format": "date" }
            },
            "required": ["employee_id", "from", "to"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "employee_id", self.name())?;
        let from = parse_date(args, "from", self.name())?;
        let to = parse_date(args, "to", self.name())?;
        if ctx.role == Role::Operator && ctx.user_id != id {
            return Err(AssistantError::Forbidden(
                "Operatoren dürfen nur eigene Einsätze einsehen.".to_string(),
            ));
        }
        let entries = ctx.services.calendar.get_employee_assignments(id, from, to).await?;
        let count = entries.len();
        Ok(json!({ "entries": entries, "count": count }))
    }
}

// ── GetAssignedCrew ───────────────────────────────────────────────────────────

pub struct GetAssignedCrew;

#[async_trait]
impl Tool for GetAssignedCrew {
    fn name(&self) -> &'static str { "get_assigned_crew" }
    fn description(&self) -> &'static str {
        "Listet die zugewiesenen Mitarbeiter eines Termins (Kalendereintrag) ODER einer Anfrage. Die ID darf eine Termin- oder Anfrage-ID sein. NUR diese Liste verwenden — niemals Mitarbeiter raten."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "format": "uuid" }
            },
            "required": ["id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Operator }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        let crew = ctx.services.calendar.get_assigned_crew(id).await?;
        let count = crew.len();
        Ok(json!({ "crew": crew, "count": count }))
    }
}

// ── CreateCalendarItem ────────────────────────────────────────────────────────

pub struct CreateCalendarItem;

#[async_trait]
impl Tool for CreateCalendarItem {
    fn name(&self) -> &'static str { "create_calendar_item" }
    fn description(&self) -> &'static str {
        "Erstellt einen Kalendereintrag (z.B. Urlaub, Krankheit, Blocker, Besichtigung). Uhrzeiten als \"HH:MM\". Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "date":       { "type": "string", "format": "date" },
                "category":   { "type": "string", "minLength": 1 },
                "title":      { "type": "string", "minLength": 1 },
                "notes":      { "type": "string" },
                "end_date":   { "type": "string", "format": "date" },
                "start_time": { "type": "string", "description": "Uhrzeit HH:MM" },
                "end_time":   { "type": "string", "description": "Uhrzeit HH:MM" },
                "location":   { "type": "string", "description": "Adresse / Ort" }
            },
            "required": ["date", "category", "title"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let date = parse_date(args, "date", self.name())?;
        let category = parse_str(args, "category", self.name())?;
        let title = parse_str(args, "title", self.name())?;
        let notes = args["notes"].as_str();
        let end_date = args["end_date"].as_str().and_then(|s| s.parse::<NaiveDate>().ok());
        let start_time = parse_time_opt(args["start_time"].as_str());
        let end_time = parse_time_opt(args["end_time"].as_str());
        let location = args["location"].as_str();
        let item = ctx
            .services
            .calendar
            .create_item(date, category, title, notes, end_date, start_time, end_time, location)
            .await?;
        Ok(serde_json::to_value(&item)?)
    }
}

// ── UpdateCalendarItem ────────────────────────────────────────────────────────

pub struct UpdateCalendarItem;

#[async_trait]
impl Tool for UpdateCalendarItem {
    fn name(&self) -> &'static str { "update_calendar_item" }
    fn description(&self) -> &'static str {
        "Aktualisiert Felder eines Kalendereintrags (inkl. Start-/Endzeit und Ort). Uhrzeiten als \"HH:MM\". Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id":    { "type": "string", "format": "uuid" },
                "patch": {
                    "type": "object",
                    "properties": {
                        "title":          { "type": "string" },
                        "category":       { "type": "string" },
                        "scheduled_date": { "type": "string", "format": "date" },
                        "end_date":       { "type": "string", "format": "date" },
                        "notes":          { "type": "string" },
                        "start_time":     { "type": "string", "description": "Uhrzeit HH:MM" },
                        "end_time":       { "type": "string", "description": "Uhrzeit HH:MM" },
                        "location":       { "type": "string", "description": "Adresse / Ort" }
                    }
                }
            },
            "required": ["id", "patch"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        // Build the patch manually so time fields accept flexible "HH:MM" input
        // (chrono's serde expects "HH:MM:SS" and would reject "10:00").
        let p = &args["patch"];
        let patch = aust_core::services::CalendarItemPatch {
            title: p["title"].as_str().map(str::to_string),
            category: p["category"].as_str().map(str::to_string),
            scheduled_date: p["scheduled_date"].as_str().and_then(|s| s.parse::<NaiveDate>().ok()),
            end_date: p["end_date"].as_str().and_then(|s| s.parse::<NaiveDate>().ok()),
            notes: p["notes"].as_str().map(str::to_string),
            start_time: parse_time_opt(p["start_time"].as_str()),
            end_time: parse_time_opt(p["end_time"].as_str()),
            location: p["location"].as_str().map(str::to_string),
        };
        let item = ctx.services.calendar.update_item(id, patch).await?;
        Ok(serde_json::to_value(&item)?)
    }
}

// ── DeleteCalendarItem (Confirm) ──────────────────────────────────────────────

pub struct DeleteCalendarItem;

#[async_trait]
impl Tool for DeleteCalendarItem {
    fn name(&self) -> &'static str { "delete_calendar_item" }
    fn description(&self) -> &'static str {
        "Löscht einen Kalendereintrag. Erfordert Bestätigung."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string", "format": "uuid" } },
            "required": ["id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    fn summarize(&self, args: &Value) -> String {
        let id = args["id"].as_str().unwrap_or("?");
        format!("Kalendereintrag {id} löschen?")
    }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        if !ctx.confirmed {
            return Ok(pending_confirmation(self.name(), args, self.summarize(args)));
        }
        ctx.services.calendar.delete_item(id).await?;
        Ok(json!({ "status": "deleted", "id": id }))
    }
}

// ── ScheduleInquiry ───────────────────────────────────────────────────────────

pub struct ScheduleInquiry;

#[async_trait]
impl Tool for ScheduleInquiry {
    fn name(&self) -> &'static str { "schedule_inquiry" }
    fn description(&self) -> &'static str {
        "Plant eine Anfrage erstmalig ein: setzt Datum + Crew, ändert den Status auf 'scheduled' UND legt einen Kalendereintrag an. NUR zum erstmaligen Verplanen verwenden. Wenn die Anfrage schon geplant ist und nur die Crew geändert werden soll, set_inquiry_crew nutzen (legt keinen doppelten Kalendereintrag an). Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "date":       { "type": "string", "format": "date" },
                "crew":       { "type": "array", "items": { "type": "string", "format": "uuid" } },
                "notes":      { "type": "string" },
                "start_time": { "type": "string", "description": "Uhrzeit HH:MM" },
                "end_time":   { "type": "string", "description": "Uhrzeit HH:MM" }
            },
            "required": ["inquiry_id", "date", "crew"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id = parse_uuid(args, "inquiry_id", self.name())?;
        let date = parse_date(args, "date", self.name())?;
        let crew: Vec<Uuid> = args["crew"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().and_then(|s| s.parse().ok())).collect())
            .unwrap_or_default();
        let notes = args["notes"].as_str();
        let start_time = parse_time_opt(args["start_time"].as_str());
        let end_time = parse_time_opt(args["end_time"].as_str());
        let item = ctx
            .services
            .calendar
            .schedule_inquiry(inquiry_id, date, crew, notes, start_time, end_time)
            .await?;
        Ok(serde_json::to_value(&item)?)
    }
}

// ── SetInquiryCrew ────────────────────────────────────────────────────────────

pub struct SetInquiryCrew;

#[async_trait]
impl Tool for SetInquiryCrew {
    fn name(&self) -> &'static str { "set_inquiry_crew" }
    fn description(&self) -> &'static str {
        "Setzt die zugewiesene Crew einer ANFRAGE (Auftrag, kind=\"auftrag\"). Ersetzt die bestehende Crew vollständig. Ändert NICHT den Status und legt KEINEN neuen Kalendereintrag an. Ohne 'date' wird das geplante Datum der Anfrage verwendet. Für Kalendereinträge (kind=\"termin\") stattdessen reassign_termin/assign_employee nutzen. Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inquiry_id": { "type": "string", "format": "uuid" },
                "crew":       { "type": "array", "items": { "type": "string", "format": "uuid" } },
                "date":       { "type": "string", "format": "date" }
            },
            "required": ["inquiry_id", "crew"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let inquiry_id = parse_uuid(args, "inquiry_id", self.name())?;
        let crew: Vec<Uuid> = args["crew"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().and_then(|s| s.parse().ok())).collect())
            .unwrap_or_default();
        let date = args["date"].as_str().and_then(|s| s.parse::<NaiveDate>().ok());
        let crew = ctx.services.calendar.set_inquiry_crew(inquiry_id, crew, date).await?;
        let count = crew.len();
        Ok(json!({ "crew": crew, "count": count }))
    }
}

// ── ReassignTermin ────────────────────────────────────────────────────────────

pub struct ReassignTermin;

#[async_trait]
impl Tool for ReassignTermin {
    fn name(&self) -> &'static str { "reassign_termin" }
    fn description(&self) -> &'static str {
        "Verschiebt einen KALENDEREINTRAG (kind=\"termin\", calendar_item-ID) auf ein anderes Datum und/oder ändert dessen Crew. Funktioniert NUR mit Termin-IDs, nicht mit Anfrage-IDs — für Aufträge (kind=\"auftrag\") set_inquiry_crew verwenden. Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "termin_id": { "type": "string", "format": "uuid" },
                "new_date":  { "type": "string", "format": "date" },
                "new_crew":  { "type": "array", "items": { "type": "string", "format": "uuid" } }
            },
            "required": ["termin_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let termin_id = parse_uuid(args, "termin_id", self.name())?;
        let new_date = args["new_date"].as_str().and_then(|s| s.parse::<NaiveDate>().ok());
        let new_crew: Option<Vec<Uuid>> = args["new_crew"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().and_then(|s| s.parse().ok())).collect());
        let item = ctx.services.calendar.reassign_termin(termin_id, new_date, new_crew).await?;
        Ok(serde_json::to_value(&item)?)
    }
}

// ── CancelTermin (Confirm) ────────────────────────────────────────────────────

pub struct CancelTermin;

#[async_trait]
impl Tool for CancelTermin {
    fn name(&self) -> &'static str { "cancel_termin" }
    fn description(&self) -> &'static str {
        "Storniert einen geplanten Termin. Erfordert Bestätigung."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id":     { "type": "string", "format": "uuid" },
                "reason": { "type": "string", "minLength": 1 }
            },
            "required": ["id", "reason"]
        })
    }
    fn safety(&self) -> Safety { Safety::Confirm }
    fn min_role(&self) -> Role { Role::Owner }

    fn summarize(&self, args: &Value) -> String {
        let id = args["id"].as_str().unwrap_or("?");
        let reason = args["reason"].as_str().unwrap_or("ohne Grund");
        format!("Termin {id} stornieren? Grund: {reason}")
    }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let id = parse_uuid(args, "id", self.name())?;
        let reason = parse_str(args, "reason", self.name())?;
        if !ctx.confirmed {
            return Ok(pending_confirmation(self.name(), args, self.summarize(args)));
        }
        ctx.services.calendar.cancel_termin(id, reason).await?;
        Ok(json!({ "status": "canceled", "id": id }))
    }
}

// ── AssignEmployee ────────────────────────────────────────────────────────────

pub struct AssignEmployee;

#[async_trait]
impl Tool for AssignEmployee {
    fn name(&self) -> &'static str { "assign_employee" }
    fn description(&self) -> &'static str {
        "Weist einen Mitarbeiter einem Kalendereintrag zu. Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "calendar_item_id": { "type": "string", "format": "uuid" },
                "employee_id":      { "type": "string", "format": "uuid" }
            },
            "required": ["calendar_item_id", "employee_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let item_id = parse_uuid(args, "calendar_item_id", self.name())?;
        let emp_id = parse_uuid(args, "employee_id", self.name())?;
        ctx.services.calendar.assign_employee(item_id, emp_id).await?;
        Ok(json!({ "ok": true }))
    }
}

// ── SetEmployeeSchedule ───────────────────────────────────────────────────────

pub struct SetEmployeeSchedule;

#[async_trait]
impl Tool for SetEmployeeSchedule {
    fn name(&self) -> &'static str { "set_employee_schedule" }
    fn description(&self) -> &'static str {
        "Setzt für EINEN zugewiesenen Mitarbeiter Datum/Uhrzeiten/Planstunden auf einem Termin ODER Auftrag. 'parent_id' ist die Termin- oder Anfrage-ID (wie in get_assigned_crew), 'employee_id' der Mitarbeiter. Nur gesetzte Felder werden geändert. Uhrzeiten als \"HH:MM\". Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "parent_id":     { "type": "string", "format": "uuid", "description": "Termin- oder Anfrage-ID" },
                "employee_id":   { "type": "string", "format": "uuid" },
                "job_date":      { "type": "string", "format": "date" },
                "start_time":    { "type": "string", "description": "Uhrzeit HH:MM" },
                "end_time":      { "type": "string", "description": "Uhrzeit HH:MM" },
                "planned_hours": { "type": "number", "minimum": 0 }
            },
            "required": ["parent_id", "employee_id"]
        })
    }
    fn safety(&self) -> Safety { Safety::Write }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let parent_id = parse_uuid(args, "parent_id", self.name())?;
        let employee_id = parse_uuid(args, "employee_id", self.name())?;
        let patch = aust_core::services::EmployeeSchedulePatch {
            job_date: args["job_date"].as_str().and_then(|s| s.parse::<NaiveDate>().ok()),
            start_time: parse_time_opt(args["start_time"].as_str()),
            end_time: parse_time_opt(args["end_time"].as_str()),
            planned_hours: args["planned_hours"].as_f64(),
        };
        let crew = ctx
            .services
            .calendar
            .set_employee_schedule(parent_id, employee_id, patch)
            .await?;
        let count = crew.len();
        Ok(json!({ "crew": crew, "count": count }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testing;
    use std::sync::Arc;

    fn dangling_pool() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid_user:invalid@127.0.0.1:1/invalid")
            .expect("lazy connect cannot fail")
    }

    fn ctx(services: aust_core::services::ServiceBundle) -> ToolCtx {
        ToolCtx {
            db: dangling_pool(),
            llm: Arc::new(crate::llm::MockAssistantLlm::always("ok")),
            services,
            role: Role::Owner,
            user_id: uuid::Uuid::nil(),
            chat_id: 0,
            session_id: uuid::Uuid::nil(),
            confirmed: false,
        }
    }

    #[tokio::test]
    async fn find_slots_returns_results() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = FindAvailableSlots
            .execute(&ctx(services), &json!({ "earliest": "2026-06-01", "latest": "2026-06-30" }))
            .await
            .unwrap();
        assert_eq!(result["count"], json!(1));
    }

    #[tokio::test]
    async fn get_employee_assignments_returns_entries() {
        let employee_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = GetEmployeeAssignments
            .execute(
                &ctx(services),
                &json!({ "employee_id": employee_id, "from": "2026-06-01", "to": "2026-06-30" }),
            )
            .await
            .unwrap();
        assert_eq!(result["count"], json!(1));
    }

    #[tokio::test]
    async fn create_calendar_item_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = CreateCalendarItem
            .execute(
                &ctx(services),
                &json!({ "date": "2026-06-15", "category": "vacation", "title": "Urlaub Anna" }),
            )
            .await
            .unwrap();
        assert_eq!(result["title"], json!("Urlaub Anna"));
    }

    #[tokio::test]
    async fn update_calendar_item_ok() {
        let id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = UpdateCalendarItem
            .execute(&ctx(services), &json!({ "id": id, "patch": { "title": "Neu" } }))
            .await
            .unwrap();
        assert_eq!(result["title"], json!("Neu"));
    }

    #[tokio::test]
    async fn delete_calendar_item_pending() {
        let id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = DeleteCalendarItem.execute(&ctx(services), &json!({ "id": id })).await.unwrap();
        assert_eq!(result["status"], json!("pending_confirmation"));
    }

    #[tokio::test]
    async fn schedule_inquiry_ok() {
        let inquiry_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(inquiry_id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = ScheduleInquiry
            .execute(
                &ctx(services),
                &json!({ "inquiry_id": inquiry_id, "date": "2026-06-15", "crew": [] }),
            )
            .await
            .unwrap();
        assert_eq!(result["category"], json!("moving"));
    }

    #[tokio::test]
    async fn reassign_termin_ok() {
        let termin_id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = ReassignTermin
            .execute(&ctx(services), &json!({ "termin_id": termin_id, "new_date": "2026-07-01" }))
            .await
            .unwrap();
        assert_eq!(result["scheduled_date"], json!("2026-07-01"));
    }

    #[tokio::test]
    async fn cancel_termin_pending() {
        let id = uuid::Uuid::new_v4();
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = CancelTermin
            .execute(&ctx(services), &json!({ "id": id, "reason": "x" }))
            .await
            .unwrap();
        assert_eq!(result["status"], json!("pending_confirmation"));
    }

    #[tokio::test]
    async fn assign_employee_ok() {
        let services = testing::mock_bundle(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = AssignEmployee
            .execute(
                &ctx(services),
                &json!({ "calendar_item_id": uuid::Uuid::new_v4(), "employee_id": uuid::Uuid::new_v4() }),
            )
            .await
            .unwrap();
        assert_eq!(result["ok"], json!(true));
    }
}
