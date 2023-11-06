// Copyright 2023 RisingWave Labs
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use itertools::Itertools;
use risingwave_common::catalog::{ColumnDesc, ColumnId};
use risingwave_common::error::{ErrorCode, Result, RwError};
use risingwave_common::types::DataType;
use risingwave_common::util::iter_util::zip_eq_fast;
use risingwave_sqlparser::ast::{
    Array, BinaryOperator, DataType as AstDataType, Expr, Function, JsonPredicateType, ObjectName,
    Query, StructField, TrimWhereField, UnaryOperator,
};

use crate::binder::expr::function::SYS_FUNCTION_WITHOUT_ARGS;
use crate::binder::Binder;
use crate::expr::{Expr as _, ExprImpl, ExprType, FunctionCall, InputRef, Parameter, SubqueryKind};

mod binary_op;
mod column;
mod function;
mod order_by;
mod subquery;
mod value;

impl Binder {
    pub fn bind_expr(&mut self, expr: Expr) -> Result<ExprImpl> {
        // We use a different function instead `map_err` directly in `bind_expr_inner`, because in
        // some cases, recursive error messages don't look good. Whole expr-level should be enough
        // in most cases.
        //
        // e.g., too verbose:
        //
        // ```ignore
        // Bind error: failed to bind expression: a1 + b1 = c1
        //
        // Caused by:
        //   Bind error: failed to bind expression: a1 + b1
        //
        // Caused by:
        //   Bind error: failed to bind expression: a1
        //
        // Caused by:
        //   Item not found: Invalid column: a1
        // ```
        //
        // confusing message with an unused subexpr, when the expr is rewritten while binding:
        //
        // ```ignore
        // > create table t (v1 int);
        // > select (case v1 when 1 then 1 when true then 2 else 0.0 end) from t;
        //
        // Bind error: failed to bind expression: CASE v1 WHEN 1 THEN 1 WHEN true THEN 2 ELSE 0.0 END
        //
        // Caused by:
        //   Bind error: failed to bind expression: v1 = true
        //
        // Caused by:
        //   Feature is not yet implemented: Equal[Int32, Boolean]
        // ```
        self.bind_expr_inner(expr.clone()).map_err(|e| {
            RwError::from(ErrorCode::BindError(format!(
                "failed to bind expression: {}\n\nCaused by:\n  {}",
                expr, e
            )))
        })
    }

    fn bind_expr_inner(&mut self, expr: Expr) -> Result<ExprImpl> {
        match expr {
            // literal
            Expr::Value(v) => Ok(ExprImpl::Literal(Box::new(self.bind_value(v)?))),
            Expr::TypedString { data_type, value } => {
                let s: ExprImpl = self.bind_string(value)?.into();
                s.cast_explicit(bind_data_type(&data_type)?)
                    .map_err(Into::into)
            }
            Expr::Row(exprs) => self.bind_row(exprs),
            // input ref
            Expr::Identifier(ident) => {
                if SYS_FUNCTION_WITHOUT_ARGS
                    .iter()
                    .any(|e| ident.real_value().as_str() == *e)
                {
                    // Rewrite a system variable to a function call, e.g. `SELECT current_schema;`
                    // will be rewritten to `SELECT current_schema();`.
                    // NOTE: Here we don't 100% follow the behavior of Postgres, as it doesn't
                    // allow `session_user()` while we do.
                    self.bind_function(Function::no_arg(ObjectName(vec![ident])))
                } else if let Some(ref lambda_args) = self.context.lambda_args {
                    // We don't support capture, so if the expression is in the lambda context,
                    // we'll not bind it for table columns.
                    if let Some((arg_idx, arg_type)) = lambda_args.get(&ident.real_value()) {
                        Ok(InputRef::new(*arg_idx, arg_type.clone()).into())
                    } else {
                        Err(
                            ErrorCode::ItemNotFound(format!("Unknown arg: {}", ident.real_value()))
                                .into(),
                        )
                    }
                } else {
                    self.bind_column(&[ident])
                }
            }
            Expr::CompoundIdentifier(idents) => self.bind_column(&idents),
            Expr::FieldIdentifier(field_expr, idents) => {
                self.bind_single_field_column(*field_expr, &idents)
            }
            // operators & functions
            Expr::UnaryOp { op, expr } => self.bind_unary_expr(op, *expr),
            Expr::BinaryOp { left, op, right } => self.bind_binary_op(*left, op, *right),
            Expr::Nested(expr) => self.bind_expr_inner(*expr),
            Expr::Array(Array { elem: exprs, .. }) => self.bind_array(exprs),
            Expr::ArrayIndex { obj, index } => self.bind_array_index(*obj, *index),
            Expr::ArrayRangeIndex { obj, start, end } => {
                self.bind_array_range_index(*obj, start, end)
            }
            Expr::Function(f) => self.bind_function(f),
            Expr::Subquery(q) => self.bind_subquery_expr(*q, SubqueryKind::Scalar),
            Expr::Exists(q) => self.bind_subquery_expr(*q, SubqueryKind::Existential),
            Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => self.bind_in_subquery(*expr, *subquery, negated),
            // special syntax (except date/time or string)
            Expr::Cast { expr, data_type } => self.bind_cast(*expr, data_type),
            Expr::IsNull(expr) => self.bind_is_operator(ExprType::IsNull, *expr),
            Expr::IsNotNull(expr) => self.bind_is_operator(ExprType::IsNotNull, *expr),
            Expr::IsTrue(expr) => self.bind_is_operator(ExprType::IsTrue, *expr),
            Expr::IsNotTrue(expr) => self.bind_is_operator(ExprType::IsNotTrue, *expr),
            Expr::IsFalse(expr) => self.bind_is_operator(ExprType::IsFalse, *expr),
            Expr::IsNotFalse(expr) => self.bind_is_operator(ExprType::IsNotFalse, *expr),
            Expr::IsUnknown(expr) => self.bind_is_unknown(ExprType::IsNull, *expr),
            Expr::IsNotUnknown(expr) => self.bind_is_unknown(ExprType::IsNotNull, *expr),
            Expr::IsDistinctFrom(left, right) => self.bind_distinct_from(*left, *right),
            Expr::IsNotDistinctFrom(left, right) => self.bind_not_distinct_from(*left, *right),
            Expr::IsJson {
                expr,
                negated,
                item_type,
                unique_keys: false,
            } => self.bind_is_json(*expr, negated, item_type),
            Expr::Case {
                operand,
                conditions,
                results,
                else_result,
            } => self.bind_case(operand, conditions, results, else_result),
            Expr::Between {
                expr,
                negated,
                low,
                high,
            } => self.bind_between(*expr, negated, *low, *high),
            Expr::InList {
                expr,
                list,
                negated,
            } => self.bind_in_list(*expr, list, negated),
            // special syntax for date/time
            Expr::Extract { field, expr } => self.bind_extract(field, *expr),
            Expr::AtTimeZone {
                timestamp,
                time_zone,
            } => self.bind_at_time_zone(*timestamp, time_zone),
            // special syntax for string
            Expr::Trim {
                expr,
                trim_where,
                trim_what,
            } => self.bind_trim(*expr, trim_where, trim_what),
            Expr::Substring {
                expr,
                substring_from,
                substring_for,
            } => self.bind_substring(*expr, substring_from, substring_for),
            Expr::Position { substring, string } => self.bind_position(*substring, *string),
            Expr::Overlay {
                expr,
                new_substring,
                start,
                count,
            } => self.bind_overlay(*expr, *new_substring, *start, count),
            Expr::Parameter { index } => self.bind_parameter(index),
            Expr::Collate { expr, collation } => self.bind_collate(*expr, collation),
            _ => Err(ErrorCode::NotImplemented(
                format!("unsupported expression {:?}", expr),
                112.into(),
            )
            .into()),
        }
    }

    pub(super) fn bind_extract(&mut self, field: String, expr: Expr) -> Result<ExprImpl> {
        let arg = self.bind_expr_inner(expr)?;
        let arg_type = arg.return_type();
        Ok(FunctionCall::new(
            ExprType::Extract,
            vec![self.bind_string(field.clone())?.into(), arg],
        )
        .map_err(|_| {
            ErrorCode::NotImplemented(
                format!(
                    "function extract({} from {:?}) doesn't exist",
                    field, arg_type
                ),
                112.into(),
            )
        })?
        .into())
    }

    pub(super) fn bind_at_time_zone(&mut self, input: Expr, time_zone: String) -> Result<ExprImpl> {
        let input = self.bind_expr_inner(input)?;
        let time_zone = self.bind_string(time_zone)?.into();
        FunctionCall::new(ExprType::AtTimeZone, vec![input, time_zone]).map(Into::into)
    }

    pub(super) fn bind_in_list(
        &mut self,
        expr: Expr,
        list: Vec<Expr>,
        negated: bool,
    ) -> Result<ExprImpl> {
        let left = self.bind_expr_inner(expr)?;
        let mut bound_expr_list = vec![left.clone()];
        let mut non_const_exprs = vec![];
        for elem in list {
            let expr = self.bind_expr_inner(elem)?;
            match expr.is_const() {
                true => bound_expr_list.push(expr),
                false => non_const_exprs.push(expr),
            }
        }
        let mut ret = FunctionCall::new(ExprType::In, bound_expr_list)?.into();
        // Non-const exprs are not part of IN-expr in backend and rewritten into OR-Equal-exprs.
        for expr in non_const_exprs {
            ret = FunctionCall::new(
                ExprType::Or,
                vec![
                    ret,
                    FunctionCall::new(ExprType::Equal, vec![left.clone(), expr])?.into(),
                ],
            )?
            .into();
        }
        if negated {
            Ok(FunctionCall::new_unchecked(ExprType::Not, vec![ret], DataType::Boolean).into())
        } else {
            Ok(ret)
        }
    }

    pub(super) fn bind_in_subquery(
        &mut self,
        expr: Expr,
        subquery: Query,
        negated: bool,
    ) -> Result<ExprImpl> {
        let bound_expr = self.bind_expr_inner(expr)?;
        let bound_subquery = self.bind_subquery_expr(subquery, SubqueryKind::In(bound_expr))?;
        if negated {
            Ok(
                FunctionCall::new_unchecked(ExprType::Not, vec![bound_subquery], DataType::Boolean)
                    .into(),
            )
        } else {
            Ok(bound_subquery)
        }
    }

    pub(super) fn bind_is_json(
        &mut self,
        expr: Expr,
        negated: bool,
        item_type: JsonPredicateType,
    ) -> Result<ExprImpl> {
        let mut args = vec![self.bind_expr_inner(expr)?];
        // Avoid `JsonPredicateType::to_string` so that we decouple sqlparser from expr execution
        let type_symbol = match item_type {
            JsonPredicateType::Value => None,
            JsonPredicateType::Array => Some("ARRAY"),
            JsonPredicateType::Object => Some("OBJECT"),
            JsonPredicateType::Scalar => Some("SCALAR"),
        };
        if let Some(s) = type_symbol {
            args.push(ExprImpl::literal_varchar(s.into()));
        }

        let is_json = FunctionCall::new(ExprType::IsJson, args)?.into();
        if negated {
            Ok(FunctionCall::new(ExprType::Not, vec![is_json])?.into())
        } else {
            Ok(is_json)
        }
    }

    pub(super) fn bind_unary_expr(&mut self, op: UnaryOperator, expr: Expr) -> Result<ExprImpl> {
        let func_type = match op {
            UnaryOperator::Not => ExprType::Not,
            UnaryOperator::Minus => ExprType::Neg,
            UnaryOperator::PGAbs => ExprType::Abs,
            UnaryOperator::PGBitwiseNot => ExprType::BitwiseNot,
            UnaryOperator::Plus => {
                return self.rewrite_positive(expr);
            }
            UnaryOperator::PGSquareRoot => ExprType::Sqrt,
            UnaryOperator::PGCubeRoot => ExprType::Cbrt,
            _ => {
                return Err(ErrorCode::NotImplemented(
                    format!("unsupported unary expression: {:?}", op),
                    112.into(),
                )
                .into())
            }
        };
        let expr = self.bind_expr_inner(expr)?;
        FunctionCall::new(func_type, vec![expr]).map(|f| f.into())
    }

    /// Directly returns the expression itself if it is a positive number.
    fn rewrite_positive(&mut self, expr: Expr) -> Result<ExprImpl> {
        let expr = self.bind_expr_inner(expr)?;
        let return_type = expr.return_type();
        if return_type.is_numeric() {
            return Ok(expr);
        }
        Err(ErrorCode::InvalidInputSyntax(format!("+ {:?}", return_type)).into())
    }

    pub(super) fn bind_trim(
        &mut self,
        expr: Expr,
        // BOTH | LEADING | TRAILING
        trim_where: Option<TrimWhereField>,
        trim_what: Option<Box<Expr>>,
    ) -> Result<ExprImpl> {
        let mut inputs = vec![self.bind_expr_inner(expr)?];
        let func_type = match trim_where {
            Some(TrimWhereField::Both) => ExprType::Trim,
            Some(TrimWhereField::Leading) => ExprType::Ltrim,
            Some(TrimWhereField::Trailing) => ExprType::Rtrim,
            None => ExprType::Trim,
        };
        if let Some(t) = trim_what {
            inputs.push(self.bind_expr_inner(*t)?);
        }
        Ok(FunctionCall::new(func_type, inputs)?.into())
    }

    fn bind_substring(
        &mut self,
        expr: Expr,
        substring_from: Option<Box<Expr>>,
        substring_for: Option<Box<Expr>>,
    ) -> Result<ExprImpl> {
        let mut args = vec![
            self.bind_expr_inner(expr)?,
            match substring_from {
                Some(expr) => self.bind_expr_inner(*expr)?,
                None => ExprImpl::literal_int(1),
            },
        ];
        if let Some(expr) = substring_for {
            args.push(self.bind_expr_inner(*expr)?);
        }
        FunctionCall::new(ExprType::Substr, args).map(|f| f.into())
    }

    fn bind_position(&mut self, substring: Expr, string: Expr) -> Result<ExprImpl> {
        let args = vec![
            // Note that we reverse the order of arguments.
            self.bind_expr_inner(string)?,
            self.bind_expr_inner(substring)?,
        ];
        FunctionCall::new(ExprType::Position, args).map(Into::into)
    }

    fn bind_overlay(
        &mut self,
        expr: Expr,
        new_substring: Expr,
        start: Expr,
        count: Option<Box<Expr>>,
    ) -> Result<ExprImpl> {
        let mut args = vec![
            self.bind_expr_inner(expr)?,
            self.bind_expr_inner(new_substring)?,
            self.bind_expr_inner(start)?,
        ];
        if let Some(count) = count {
            args.push(self.bind_expr_inner(*count)?);
        }
        FunctionCall::new(ExprType::Overlay, args).map(|f| f.into())
    }

    fn bind_parameter(&mut self, index: u64) -> Result<ExprImpl> {
        Ok(Parameter::new(index, self.param_types.clone()).into())
    }

    /// Bind `expr (not) between low and high`
    pub(super) fn bind_between(
        &mut self,
        expr: Expr,
        negated: bool,
        low: Expr,
        high: Expr,
    ) -> Result<ExprImpl> {
        let expr = self.bind_expr_inner(expr)?;
        let low = self.bind_expr_inner(low)?;
        let high = self.bind_expr_inner(high)?;

        let func_call = if negated {
            // negated = true: expr < low or expr > high
            FunctionCall::new_unchecked(
                ExprType::Or,
                vec![
                    FunctionCall::new(ExprType::LessThan, vec![expr.clone(), low])?.into(),
                    FunctionCall::new(ExprType::GreaterThan, vec![expr, high])?.into(),
                ],
                DataType::Boolean,
            )
        } else {
            // negated = false: expr >= low and expr <= high
            FunctionCall::new_unchecked(
                ExprType::And,
                vec![
                    FunctionCall::new(ExprType::GreaterThanOrEqual, vec![expr.clone(), low])?
                        .into(),
                    FunctionCall::new(ExprType::LessThanOrEqual, vec![expr, high])?.into(),
                ],
                DataType::Boolean,
            )
        };

        Ok(func_call.into())
    }

    pub(super) fn bind_case(
        &mut self,
        operand: Option<Box<Expr>>,
        conditions: Vec<Expr>,
        results: Vec<Expr>,
        else_result: Option<Box<Expr>>,
    ) -> Result<ExprImpl> {
        let mut inputs = Vec::new();
        let results_expr: Vec<ExprImpl> = results
            .into_iter()
            .map(|expr| self.bind_expr_inner(expr))
            .collect::<Result<_>>()?;
        let else_result_expr = else_result
            .map(|expr| self.bind_expr_inner(*expr))
            .transpose()?;

        for (condition, result) in zip_eq_fast(conditions, results_expr) {
            let condition = match operand {
                Some(ref t) => Expr::BinaryOp {
                    left: t.clone(),
                    op: BinaryOperator::Eq,
                    right: Box::new(condition),
                },
                None => condition,
            };
            inputs.push(
                self.bind_expr_inner(condition)
                    .and_then(|expr| expr.enforce_bool_clause("CASE WHEN"))?,
            );
            inputs.push(result);
        }
        if let Some(expr) = else_result_expr {
            inputs.push(expr);
        }
        if inputs.iter().any(ExprImpl::has_table_function) {
            return Err(
                ErrorCode::BindError("table functions are not allowed in CASE".into()).into(),
            );
        }
        Ok(FunctionCall::new(ExprType::Case, inputs)?.into())
    }

    pub(super) fn bind_is_operator(&mut self, func_type: ExprType, expr: Expr) -> Result<ExprImpl> {
        let expr = self.bind_expr_inner(expr)?;
        Ok(FunctionCall::new(func_type, vec![expr])?.into())
    }

    pub(super) fn bind_is_unknown(&mut self, func_type: ExprType, expr: Expr) -> Result<ExprImpl> {
        let expr = self
            .bind_expr_inner(expr)?
            .cast_implicit(DataType::Boolean)?;
        Ok(FunctionCall::new(func_type, vec![expr])?.into())
    }

    pub(super) fn bind_distinct_from(&mut self, left: Expr, right: Expr) -> Result<ExprImpl> {
        let left = self.bind_expr_inner(left)?;
        let right = self.bind_expr_inner(right)?;
        let func_call = FunctionCall::new(ExprType::IsDistinctFrom, vec![left, right]);
        Ok(func_call?.into())
    }

    pub(super) fn bind_not_distinct_from(&mut self, left: Expr, right: Expr) -> Result<ExprImpl> {
        let left = self.bind_expr_inner(left)?;
        let right = self.bind_expr_inner(right)?;
        let func_call = FunctionCall::new(ExprType::IsNotDistinctFrom, vec![left, right]);
        Ok(func_call?.into())
    }

    pub(super) fn bind_cast(&mut self, expr: Expr, data_type: AstDataType) -> Result<ExprImpl> {
        match &data_type {
            // Casting to Regclass type means getting the oid of expr.
            // See https://www.postgresql.org/docs/current/datatype-oid.html.
            // Currently only string liter expr is supported since we cannot handle subquery in join
            // on condition: https://github.com/risingwavelabs/risingwave/issues/6852
            // TODO: Add generic expr support when needed
            AstDataType::Regclass => {
                let input = self.bind_expr_inner(expr)?;
                match input.return_type() {
                    DataType::Varchar => Ok(ExprImpl::FunctionCall(Box::new(
                        FunctionCall::new_unchecked(
                            ExprType::CastRegclass,
                            vec![input],
                            DataType::Int32,
                        ),
                    ))),
                    DataType::Int32 => Ok(input),
                    dt if dt.is_int() => Ok(input.cast_explicit(DataType::Int32)?),
                    _ => Err(ErrorCode::BindError("Unsupported input type".to_string()).into()),
                }
            }
            AstDataType::Regproc => {
                let lhs = self.bind_expr_inner(expr)?;
                let lhs_ty = lhs.return_type();
                if lhs_ty == DataType::Varchar {
                    // FIXME: Currently, we only allow VARCHAR to be casted to Regproc.
                    // FIXME: Check whether it's a valid proc
                    // FIXME: The return type should be casted to Regproc, but we don't have this type.
                    Ok(lhs)
                } else {
                    Err(ErrorCode::BindError(format!("Can't cast {} to regproc", lhs_ty)).into())
                }
            }
            _ => self.bind_cast_inner(expr, bind_data_type(&data_type)?),
        }
    }

    pub fn bind_cast_inner(&mut self, expr: Expr, data_type: DataType) -> Result<ExprImpl> {
        if let Expr::Array(Array { elem: ref expr, .. }) = expr
            && matches!(&data_type, DataType::List { .. })
        {
            return self.bind_array_cast(expr.clone(), data_type);
        }
        let lhs = self.bind_expr_inner(expr)?;
        lhs.cast_explicit(data_type).map_err(Into::into)
    }

    pub fn bind_collate(&mut self, expr: Expr, collation: ObjectName) -> Result<ExprImpl> {
        if !["C", "POSIX"].contains(&collation.real_value().as_str()) {
            return Err(ErrorCode::NotImplemented(
                "Collate collation other than `C` or `POSIX` is not implemented".into(),
                None.into(),
            )
            .into());
        }

        let bound_inner = self.bind_expr_inner(expr)?;
        let ret_type = bound_inner.return_type();

        match ret_type {
            DataType::Varchar => {}
            _ => {
                return Err(ErrorCode::NotSupported(
                    format!("{} is not a collatable data type", ret_type),
                    "The only built-in collatable data types are `varchar`, please check your type"
                        .into(),
                )
                .into());
            }
        }

        Ok(bound_inner)
    }
}

/// Given a type `STRUCT<v1 int>`, this function binds the field `v1 int`.
pub fn bind_struct_field(column_def: &StructField) -> Result<ColumnDesc> {
    let field_descs = if let AstDataType::Struct(defs) = &column_def.data_type {
        defs.iter()
            .map(|f| {
                Ok(ColumnDesc {
                    data_type: bind_data_type(&f.data_type)?,
                    // Literals don't have `column_id`.
                    column_id: ColumnId::new(0),
                    name: f.name.real_value(),
                    field_descs: vec![],
                    type_name: "".to_string(),
                    generated_or_default_column: None,
                    description: None,
                })
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        vec![]
    };
    Ok(ColumnDesc {
        data_type: bind_data_type(&column_def.data_type)?,
        column_id: ColumnId::new(0),
        name: column_def.name.real_value(),
        field_descs,
        type_name: "".to_string(),
        generated_or_default_column: None,
        description: None,
    })
}

pub fn bind_data_type(data_type: &AstDataType) -> Result<DataType> {
    let new_err = || {
        ErrorCode::NotImplemented(
            format!("unsupported data type: {:}", data_type),
            None.into(),
        )
    };
    let data_type = match data_type {
        AstDataType::Boolean => DataType::Boolean,
        AstDataType::SmallInt => DataType::Int16,
        AstDataType::Int => DataType::Int32,
        AstDataType::BigInt => DataType::Int64,
        AstDataType::Real | AstDataType::Float(Some(1..=24)) => DataType::Float32,
        AstDataType::Double | AstDataType::Float(Some(25..=53) | None) => DataType::Float64,
        AstDataType::Float(Some(0 | 54..)) => unreachable!(),
        AstDataType::Decimal(None, None) => DataType::Decimal,
        AstDataType::Varchar | AstDataType::Text => DataType::Varchar,
        AstDataType::Date => DataType::Date,
        AstDataType::Time(false) => DataType::Time,
        AstDataType::Timestamp(false) => DataType::Timestamp,
        AstDataType::Timestamp(true) => DataType::Timestamptz,
        AstDataType::Interval => DataType::Interval,
        AstDataType::Array(datatype) => DataType::List(Box::new(bind_data_type(datatype)?)),
        AstDataType::Char(..) => {
            return Err(ErrorCode::NotImplemented(
                "CHAR is not supported, please use VARCHAR instead\n".to_string(),
                None.into(),
            )
            .into())
        }
        AstDataType::Struct(types) => DataType::new_struct(
            types
                .iter()
                .map(|f| bind_data_type(&f.data_type))
                .collect::<Result<Vec<_>>>()?,
            types.iter().map(|f| f.name.real_value()).collect_vec(),
        ),
        AstDataType::Custom(qualified_type_name) if qualified_type_name.0.len() == 1 => {
            // In PostgreSQL, these are not keywords but pre-defined names that could be extended by
            // `CREATE TYPE`.
            match qualified_type_name.0[0].real_value().as_str() {
                "int2" => DataType::Int16,
                "int4" => DataType::Int32,
                "int8" => DataType::Int64,
                "rw_int256" => DataType::Int256,
                "float4" => DataType::Float32,
                "float8" => DataType::Float64,
                "timestamptz" => DataType::Timestamptz,
                "serial" => {
                    return Err(ErrorCode::NotSupported(
                        "Column type SERIAL is not supported".into(),
                        "Please remove the SERIAL column".into(),
                    )
                    .into())
                }
                _ => return Err(new_err().into()),
            }
        }
        AstDataType::Bytea => DataType::Bytea,
        AstDataType::Jsonb => DataType::Jsonb,
        AstDataType::Regclass
        | AstDataType::Regproc
        | AstDataType::Uuid
        | AstDataType::Custom(_)
        | AstDataType::Decimal(_, _)
        | AstDataType::Time(true) => return Err(new_err().into()),
    };
    Ok(data_type)
}
