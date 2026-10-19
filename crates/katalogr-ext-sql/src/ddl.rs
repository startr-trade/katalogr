//! Parsed statements to drafts, bindings and facts (§6.2 step 3).

use crate::CONTRACT_COLUMN;
use crate::CONTRACT_ROUTINE;
use crate::CONTRACT_TABLE;
use crate::dialect::Dialect;
use crate::extract::{Builder, Owner, s};
use crate::names::{column_key, normalise_ident, routine_key, table_key};
use crate::refs::{BodyKind, Collector, as_strs, name_parts, routine_body};
use katalogr_model::{Confidence, ContractRole, Fact, Span};
use sqlparser::ast::{
    AlterColumnOperation, AlterTableOperation, ColumnDef, ColumnOption, CommentObject,
    ConditionalStatements, CreatePolicyCommand, CreatePolicyType, CreateTable, Expr, FunctionDesc,
    Ident, IndexColumn, ObjectName, ObjectType, Owner as PolicyOwner, RenameTableNameKind, Spanned,
    Statement, TableConstraint, TriggerEvent, TriggerObjectKind, TriggerPeriod,
};

const EXT: Confidence = Confidence::Extracted;

fn ident_key(i: &Ident) -> String {
    i.value.to_lowercase()
}

fn index_col(ic: &IndexColumn) -> Option<String> {
    match &ic.column.expr {
        Expr::Identifier(i) => Some(ident_key(i)),
        _ => None,
    }
}

fn args_sig(args: &Option<Vec<sqlparser::ast::OperateFunctionArg>>) -> String {
    args.as_ref()
        .map(|a| {
            a.iter()
                .map(|x| x.data_type.to_string().to_lowercase())
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

fn arg_texts(args: &Option<Vec<sqlparser::ast::OperateFunctionArg>>) -> Fact {
    Fact::List(
        args.iter()
            .flatten()
            .map(|a| match &a.name {
                Some(n) => s(&format!("{} {}", n.value, a.data_type)),
                None => s(&a.data_type.to_string()),
            })
            .collect(),
    )
}

impl Builder<'_> {
    fn rkey(&self, name: &ObjectName) -> String {
        let parts = name_parts(name);
        routine_key(&as_strs(&parts), &self.schema)
    }

    fn table_symbol(&mut self, key: &str, span: Span) -> usize {
        self.draft("table", key, "", span)
    }

    /// Apply the options of a column definition to a column draft.
    fn apply_option(&mut self, t: usize, c: usize, opt: &ColumnOption, span: Span) {
        match opt {
            ColumnOption::NotNull => self.col_fact(t, c, "nullable", Fact::Bool(false)),
            ColumnOption::Null => self.col_fact(t, c, "nullable", Fact::Bool(true)),
            ColumnOption::PrimaryKey(_) => {
                self.col_fact(t, c, "primary_key", Fact::Bool(true));
                self.col_fact(t, c, "nullable", Fact::Bool(false));
            }
            ColumnOption::Default(e) => self.col_fact(t, c, "default", s(&e.to_string())),
            ColumnOption::Comment(text) => self.col_fact(t, c, "comment", s(text)),
            ColumnOption::ForeignKey(fk) => {
                let target = self.tkey(&fk.foreign_table);
                let reference = match fk.referred_columns.first() {
                    Some(rc) => column_key(&target, &rc.value),
                    None => target.clone(),
                };
                self.col_fact(t, c, "references", s(&reference));
                self.bind(
                    Owner::Sym(t),
                    CONTRACT_TABLE,
                    &target,
                    ContractRole::Consumes,
                    span,
                    EXT,
                );
            }
            _ => {}
        }
    }

    /// A column definition: facts and the `sql_column` provides binding.
    fn add_column(&mut self, t: usize, def: &ColumnDef, stmt: Span) -> usize {
        let name = ident_key(&def.name);
        let sp = self.rel_span(def.span(), stmt);
        let c = self.column(t, &name, sp);
        self.col_fact(t, c, "type", s(&def.data_type.to_string()));
        self.col_fact(t, c, "nullable", Fact::Bool(true));
        for o in &def.options {
            self.apply_option(t, c, &o.option, stmt);
        }
        self.bind_column(t, c, ContractRole::Provides, sp);
        c
    }

    fn bind_column(&mut self, t: usize, c: usize, role: ContractRole, span: Span) {
        let key = column_key(
            &self.drafts[t].key.clone(),
            &self.drafts[t].columns[c].name.clone(),
        );
        self.bind(Owner::Col(t, c), CONTRACT_COLUMN, &key, role, span, EXT);
    }

    fn table_constraint(&mut self, t: usize, tc: &TableConstraint, stmt: Span) {
        match tc {
            TableConstraint::PrimaryKey(pk) => {
                for ic in &pk.columns {
                    if let Some(n) = index_col(ic) {
                        let c = self.column(t, &n, stmt);
                        self.col_fact(t, c, "primary_key", Fact::Bool(true));
                        self.col_fact(t, c, "nullable", Fact::Bool(false));
                    }
                }
            }
            TableConstraint::ForeignKey(fk) => {
                let target = self.tkey(&fk.foreign_table);
                for (i, col) in fk.columns.iter().enumerate() {
                    let c = self.column(t, &ident_key(col), stmt);
                    let reference = if fk.referred_columns.len() == fk.columns.len() {
                        column_key(&target, &fk.referred_columns[i].value)
                    } else {
                        target.clone()
                    };
                    self.col_fact(t, c, "references", s(&reference));
                }
                self.bind(
                    Owner::Sym(t),
                    CONTRACT_TABLE,
                    &target,
                    ContractRole::Consumes,
                    stmt,
                    EXT,
                );
            }
            _ => {}
        }
    }

    fn create_table(&mut self, ct: &CreateTable, span: Span, d: Dialect) {
        let key = self.tkey(&ct.name);
        let t = self.table_symbol(&key, span);
        self.touch(t, "create", true, Some(d));
        if ct.temporary {
            self.fact(t, "temporary", Fact::Bool(true));
        }
        self.bind(
            Owner::Sym(t),
            CONTRACT_TABLE,
            &key,
            ContractRole::Provides,
            span,
            EXT,
        );
        for def in &ct.columns {
            self.add_column(t, def, span);
        }
        for tc in &ct.constraints {
            self.table_constraint(t, tc, span);
        }
        if let Some(q) = &ct.query {
            let refs = Collector::of(q).refs(&self.schema);
            self.consume_refs(Owner::Sym(t), &refs, span, EXT);
        }
    }

    fn alter_table(&mut self, at: &sqlparser::ast::AlterTable, span: Span, d: Dialect) {
        let key = self.tkey(&at.name);
        let t = self.table_symbol(&key, span);
        self.touch(t, "alter", true, Some(d));
        self.bind(
            Owner::Sym(t),
            CONTRACT_TABLE,
            &key,
            ContractRole::Provides,
            span,
            EXT,
        );
        for op in &at.operations {
            match op {
                AlterTableOperation::AddColumn { column_def, .. } => {
                    self.add_column(t, column_def, span);
                }
                AlterTableOperation::DropColumn { column_names, .. } => {
                    for n in column_names {
                        let c = self.column(t, &ident_key(n), span);
                        self.col_fact(t, c, "dropped", Fact::Bool(true));
                        self.bind_column(t, c, ContractRole::Declares, span);
                    }
                }
                AlterTableOperation::RenameColumn {
                    old_column_name,
                    new_column_name,
                } => self.rename_column(t, old_column_name, new_column_name, None, &[], span),
                AlterTableOperation::ChangeColumn {
                    old_name,
                    new_name,
                    data_type,
                    options,
                    ..
                } => self.rename_column(t, old_name, new_name, Some(data_type), options, span),
                AlterTableOperation::ModifyColumn {
                    col_name,
                    data_type,
                    options,
                    ..
                } => {
                    let c = self.column(t, &ident_key(col_name), span);
                    self.col_fact(t, c, "type", s(&data_type.to_string()));
                    for o in options {
                        self.apply_option(t, c, o, span);
                    }
                    self.bind_column(t, c, ContractRole::Provides, span);
                }
                AlterTableOperation::AlterColumn { column_name, op } => {
                    let c = self.column(t, &ident_key(column_name), span);
                    match op {
                        AlterColumnOperation::SetDataType { data_type, .. } => {
                            self.col_fact(t, c, "type", s(&data_type.to_string()))
                        }
                        AlterColumnOperation::SetNotNull => {
                            self.col_fact(t, c, "nullable", Fact::Bool(false))
                        }
                        AlterColumnOperation::DropNotNull => {
                            self.col_fact(t, c, "nullable", Fact::Bool(true))
                        }
                        AlterColumnOperation::SetDefault { value } => {
                            self.col_fact(t, c, "default", s(&value.to_string()))
                        }
                        _ => {}
                    }
                    self.bind_column(t, c, ContractRole::Provides, span);
                }
                AlterTableOperation::AddConstraint { constraint, .. } => {
                    self.table_constraint(t, constraint, span);
                }
                AlterTableOperation::RenameTable { table_name } => {
                    let (RenameTableNameKind::As(n) | RenameTableNameKind::To(n)) = table_name;
                    let new_key = self.tkey(n);
                    self.touch(t, "rename", true, Some(d));
                    self.fact(t, "renamed_to", s(&new_key));
                    let nt = self.table_symbol(&new_key, span);
                    self.touch(nt, "rename", true, Some(d));
                    self.fact(nt, "renamed_from", s(&key));
                    self.bind(
                        Owner::Sym(nt),
                        CONTRACT_TABLE,
                        &new_key,
                        ContractRole::Provides,
                        span,
                        EXT,
                    );
                }
                AlterTableOperation::EnableRowLevelSecurity => {
                    self.fact(t, "row_level_security", Fact::Bool(true))
                }
                AlterTableOperation::DisableRowLevelSecurity => {
                    self.fact(t, "row_level_security", Fact::Bool(false))
                }
                _ => {}
            }
        }
    }

    fn rename_column(
        &mut self,
        t: usize,
        old: &Ident,
        new: &Ident,
        data_type: Option<&sqlparser::ast::DataType>,
        options: &[ColumnOption],
        span: Span,
    ) {
        let (ok, nk) = (ident_key(old), ident_key(new));
        if ok != nk {
            let oc = self.column(t, &ok, span);
            self.col_fact(t, oc, "renamed_to", s(&nk));
            self.bind_column(t, oc, ContractRole::Declares, span);
        }
        let nc = self.column(t, &nk, span);
        if ok != nk {
            self.col_fact(t, nc, "renamed_from", s(&ok));
        }
        if let Some(dt) = data_type {
            self.col_fact(t, nc, "type", s(&dt.to_string()));
        }
        for o in options {
            self.apply_option(t, nc, o, span);
        }
        self.bind_column(t, nc, ContractRole::Provides, span);
    }

    fn statements_refs(&self, stmts: &[Statement]) -> crate::scan::Refs {
        Collector::of(&stmts.to_vec()).refs(&self.schema)
    }

    fn cond_refs(&self, body: &ConditionalStatements) -> crate::scan::Refs {
        match body {
            ConditionalStatements::Sequence { statements } => self.statements_refs(statements),
            ConditionalStatements::BeginEnd(b) => self.statements_refs(&b.statements),
        }
    }

    fn desc_sig(desc: &FunctionDesc) -> String {
        args_sig(&desc.args)
    }

    /// Map one parsed statement.
    pub(crate) fn map_statement(&mut self, st: &Statement, span: Span, d: Dialect) {
        match st {
            Statement::CreateTable(ct) => self.create_table(ct, span, d),
            Statement::CreateVirtualTable { name, .. } => {
                let key = self.tkey(name);
                let t = self.table_symbol(&key, span);
                self.touch(t, "create", true, Some(d));
                self.fact(t, "virtual", Fact::Bool(true));
                self.bind(
                    Owner::Sym(t),
                    CONTRACT_TABLE,
                    &key,
                    ContractRole::Provides,
                    span,
                    EXT,
                );
            }
            Statement::AlterTable(at) => self.alter_table(at, span, d),
            Statement::CreateIndex(ci) => {
                let key = self.tkey(&ci.table_name);
                let t = self.table_symbol(&key, span);
                self.touch(t, "index", true, Some(d));
                self.bind(
                    Owner::Sym(t),
                    CONTRACT_TABLE,
                    &key,
                    ContractRole::Provides,
                    span,
                    EXT,
                );
            }
            Statement::Comment {
                object_type,
                object_name,
                comment,
                ..
            } => match object_type {
                CommentObject::Table => {
                    let key = self.tkey(object_name);
                    let t = self.table_symbol(&key, span);
                    self.touch(t, "comment", true, Some(d));
                    if let Some(c) = comment {
                        self.fact(t, "comment", s(c));
                    }
                    self.bind(
                        Owner::Sym(t),
                        CONTRACT_TABLE,
                        &key,
                        ContractRole::Provides,
                        span,
                        EXT,
                    );
                }
                CommentObject::Column => {
                    let parts = name_parts(object_name);
                    if parts.len() >= 2 {
                        let (tp, cp) = parts.split_at(parts.len() - 1);
                        let key = table_key(&as_strs(tp), &self.schema);
                        let t = self.table_symbol(&key, span);
                        self.touch(t, "comment", true, Some(d));
                        let c = self.column(t, &normalise_ident(&cp[0]), span);
                        if let Some(text) = comment {
                            self.col_fact(t, c, "comment", s(text));
                        }
                        self.bind_column(t, c, ContractRole::Provides, span);
                    }
                }
                _ => {}
            },
            Statement::CreateView(cv) => {
                let key = self.tkey(&cv.name);
                let v = self.draft("view", &key, "", span);
                self.touch(v, "create", true, Some(d));
                if cv.materialized {
                    self.fact(v, "materialized", Fact::Bool(true));
                }
                self.bind(
                    Owner::Sym(v),
                    CONTRACT_TABLE,
                    &key,
                    ContractRole::Provides,
                    span,
                    EXT,
                );
                let refs = Collector::of(&cv.query).refs(&self.schema);
                self.consume_refs(Owner::Sym(v), &refs, span, EXT);
            }
            Statement::CreateFunction(cf) => {
                let key = self.rkey(&cf.name);
                let sig = args_sig(&cf.args);
                let f = self.draft("function", &key, &sig, span);
                self.touch(f, "create", true, Some(d));
                if let Some(l) = &cf.language {
                    self.fact(f, "language", s(&l.value.to_lowercase()));
                }
                self.fact(f, "args", arg_texts(&cf.args));
                if let Some(r) = &cf.return_type {
                    let text = match r {
                        sqlparser::ast::FunctionReturnType::DataType(t) => t.to_string(),
                        sqlparser::ast::FunctionReturnType::SetOf(t) => format!("SETOF {t}"),
                    };
                    self.fact(f, "returns", s(&text));
                }
                let (refs, kind) = routine_body(cf, d, &self.schema);
                self.fact(f, "body", s(kind.as_str()));
                self.bind(
                    Owner::Sym(f),
                    CONTRACT_ROUTINE,
                    &key,
                    ContractRole::Provides,
                    span,
                    EXT,
                );
                let conf = if kind == BodyKind::Scanned {
                    Confidence::Inferred
                } else {
                    EXT
                };
                self.consume_refs(Owner::Sym(f), &refs, span, conf);
            }
            Statement::CreateProcedure {
                name,
                params,
                language,
                body,
                ..
            } => {
                let key = self.rkey(name);
                let sig = params
                    .iter()
                    .flatten()
                    .map(|p| p.data_type.to_string().to_lowercase())
                    .collect::<Vec<_>>()
                    .join(",");
                let p = self.draft("procedure", &key, &sig, span);
                self.touch(p, "create", true, Some(d));
                if let Some(l) = language {
                    self.fact(p, "language", s(&l.value.to_lowercase()));
                }
                self.fact(
                    p,
                    "args",
                    Fact::List(
                        params
                            .iter()
                            .flatten()
                            .map(|a| s(&format!("{} {}", a.name.value, a.data_type)))
                            .collect(),
                    ),
                );
                self.fact(p, "body", s("parsed"));
                self.bind(
                    Owner::Sym(p),
                    CONTRACT_ROUTINE,
                    &key,
                    ContractRole::Provides,
                    span,
                    EXT,
                );
                let refs = self.cond_refs(body);
                self.consume_refs(Owner::Sym(p), &refs, span, EXT);
            }
            Statement::CreateTrigger(ct) => {
                let table = self.tkey(&ct.table_name);
                let schema = table
                    .split_once('.')
                    .map_or(self.schema.as_str(), |(a, _)| a);
                let tname = ct.name.0.last().map(|p| match p {
                    sqlparser::ast::ObjectNamePart::Identifier(i) => ident_key(i),
                    _ => String::new(),
                });
                let key = format!("{schema}.{}", tname.unwrap_or_default());
                let tr = self.draft("trigger", &key, "", span);
                self.touch(tr, "create", true, Some(d));
                self.fact(tr, "table", s(&table));
                if let Some(p) = &ct.period {
                    let text = match p {
                        TriggerPeriod::Before => "before",
                        TriggerPeriod::InsteadOf => "instead of",
                        TriggerPeriod::After | TriggerPeriod::For => "after",
                    };
                    self.fact(tr, "timing", s(text));
                }
                self.fact(
                    tr,
                    "events",
                    Fact::List(
                        ct.events
                            .iter()
                            .map(|e| {
                                s(match e {
                                    TriggerEvent::Insert => "insert",
                                    TriggerEvent::Update(_) => "update",
                                    TriggerEvent::Delete => "delete",
                                    TriggerEvent::Truncate => "truncate",
                                })
                            })
                            .collect(),
                    ),
                );
                if let Some(TriggerObjectKind::For(o) | TriggerObjectKind::ForEach(o)) =
                    &ct.trigger_object
                {
                    self.fact(
                        tr,
                        "for_each",
                        s(match o {
                            sqlparser::ast::TriggerObject::Row => "row",
                            sqlparser::ast::TriggerObject::Statement => "statement",
                        }),
                    );
                }
                self.bind(
                    Owner::Sym(tr),
                    CONTRACT_TABLE,
                    &table,
                    ContractRole::Consumes,
                    span,
                    EXT,
                );
                if let Some(body) = &ct.exec_body {
                    let rk = self.rkey(&body.func_desc.name);
                    self.fact(tr, "executes", s(&rk));
                    self.bind(
                        Owner::Sym(tr),
                        CONTRACT_ROUTINE,
                        &rk,
                        ContractRole::Consumes,
                        span,
                        EXT,
                    );
                }
                if let Some(stmts) = &ct.statements {
                    let refs = self.cond_refs(stmts);
                    self.consume_refs(Owner::Sym(tr), &refs, span, EXT);
                }
            }
            Statement::CreatePolicy(cp) => {
                let table = self.tkey(&cp.table_name);
                let key = format!("{table}.{}", ident_key(&cp.name));
                let p = self.draft("policy", &key, "", span);
                self.touch(p, "create", true, Some(d));
                self.fact(p, "table", s(&table));
                let cmd = match cp.command {
                    Some(CreatePolicyCommand::Select) => "select",
                    Some(CreatePolicyCommand::Insert) => "insert",
                    Some(CreatePolicyCommand::Update) => "update",
                    Some(CreatePolicyCommand::Delete) => "delete",
                    Some(CreatePolicyCommand::All) | None => "all",
                };
                self.fact(p, "command", s(cmd));
                if let Some(roles) = &cp.to {
                    self.fact(
                        p,
                        "roles",
                        Fact::List(
                            roles
                                .iter()
                                .map(|r| match r {
                                    PolicyOwner::Ident(i) => s(&i.value),
                                    PolicyOwner::CurrentRole => s("current_role"),
                                    PolicyOwner::CurrentUser => s("current_user"),
                                    PolicyOwner::SessionUser => s("session_user"),
                                })
                                .collect(),
                        ),
                    );
                }
                self.fact(
                    p,
                    "permissive",
                    Fact::Bool(!matches!(
                        cp.policy_type,
                        Some(CreatePolicyType::Restrictive)
                    )),
                );
                if let Some(u) = &cp.using {
                    self.fact(p, "using", s(&u.to_string()));
                }
                if let Some(w) = &cp.with_check {
                    self.fact(p, "with_check", s(&w.to_string()));
                }
                self.bind(
                    Owner::Sym(p),
                    CONTRACT_TABLE,
                    &table,
                    ContractRole::Consumes,
                    span,
                    EXT,
                );
                let mut c = Collector::default_();
                if let Some(u) = &cp.using {
                    c.add(u);
                }
                if let Some(w) = &cp.with_check {
                    c.add(w);
                }
                let refs = c.refs(&self.schema);
                self.consume_refs(Owner::Sym(p), &refs, span, EXT);
            }
            Statement::Drop {
                object_type, names, ..
            } => {
                let kind = match object_type {
                    ObjectType::Table => "table",
                    ObjectType::View | ObjectType::MaterializedView => "view",
                    _ => return,
                };
                for n in names {
                    let key = self.tkey(n);
                    let i = self.draft(kind, &key, "", span);
                    self.touch(i, "drop", true, Some(d));
                    if *object_type == ObjectType::MaterializedView {
                        self.fact(i, "materialized", Fact::Bool(true));
                    }
                    self.bind(
                        Owner::Sym(i),
                        CONTRACT_TABLE,
                        &key,
                        ContractRole::Declares,
                        span,
                        EXT,
                    );
                }
            }
            Statement::DropFunction(df) => {
                for desc in &df.func_desc {
                    self.drop_routine("function", desc, span, d);
                }
            }
            Statement::DropProcedure { proc_desc, .. } => {
                for desc in proc_desc {
                    self.drop_routine("procedure", desc, span, d);
                }
            }
            Statement::DropTrigger(dt) => {
                let schema = match &dt.table_name {
                    Some(t) => {
                        let k = self.tkey(t);
                        k.split_once('.')
                            .map_or(self.schema.clone(), |(a, _)| a.to_string())
                    }
                    None => self.schema.clone(),
                };
                let name = dt.trigger_name.0.last().map(|p| match p {
                    sqlparser::ast::ObjectNamePart::Identifier(i) => ident_key(i),
                    _ => String::new(),
                });
                let key = format!("{schema}.{}", name.unwrap_or_default());
                let i = self.draft("trigger", &key, "", span);
                self.touch(i, "drop", true, Some(d));
            }
            Statement::DropPolicy(dp) => {
                let table = self.tkey(&dp.table_name);
                let key = format!("{table}.{}", ident_key(&dp.name));
                let i = self.draft("policy", &key, "", span);
                self.touch(i, "drop", true, Some(d));
            }
            Statement::Query(_)
            | Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)
            | Statement::Merge(_) => {
                self.dml += 1;
                let refs = Collector::of(st).refs(&self.schema);
                self.consume_refs(Owner::Artifact, &refs, span, EXT);
            }
            Statement::Call(f) => {
                self.dml += 1;
                let mut refs = Collector::of(f).refs(&self.schema);
                let key = self.rkey(&f.name);
                refs.routines.insert(key);
                self.consume_refs(Owner::Artifact, &refs, span, EXT);
            }
            _ => {}
        }
    }

    fn drop_routine(&mut self, kind: &str, desc: &FunctionDesc, span: Span, d: Dialect) {
        let key = self.rkey(&desc.name);
        let sig = Self::desc_sig(desc);
        let i = self.draft(kind, &key, &sig, span);
        self.touch(i, "drop", true, Some(d));
        self.bind(
            Owner::Sym(i),
            CONTRACT_ROUTINE,
            &key,
            ContractRole::Declares,
            span,
            EXT,
        );
    }
}
