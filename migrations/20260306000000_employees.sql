-- Employee management tables
CREATE TABLE employees (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    salutation VARCHAR(10) CHECK (salutation IN ('Herr', 'Frau', 'D')),
    first_name VARCHAR(255) NOT NULL,
    last_name VARCHAR(255) NOT NULL,
    email VARCHAR(255) NOT NULL UNIQUE,
    phone VARCHAR(50),
    monthly_hours_target DECIMAL(6,2) NOT NULL DEFAULT 160.0,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_employees_active ON employees(active);
CREATE TRIGGER update_employees_updated_at
    BEFORE UPDATE ON employees FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

CREATE TABLE inquiry_employees (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    inquiry_id UUID NOT NULL REFERENCES inquiries(id) ON DELETE CASCADE,
    employee_id UUID NOT NULL REFERENCES employees(id) ON DELETE CASCADE,
    planned_hours DECIMAL(6,2) NOT NULL DEFAULT 0.0,
    actual_hours DECIMAL(6,2),
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(inquiry_id, employee_id)
);

CREATE INDEX idx_inquiry_employees_inquiry ON inquiry_employees(inquiry_id);
CREATE INDEX idx_inquiry_employees_employee ON inquiry_employees(employee_id);
CREATE TRIGGER update_inquiry_employees_updated_at
    BEFORE UPDATE ON inquiry_employees FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
