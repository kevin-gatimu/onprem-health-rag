#!/usr/bin/env python3
"""Generate `init/02_seed.sql` for the synthetic hospital EMR.

Deterministic: the same seed produces byte-identical SQL, so the seed file can be
committed and diffed. Nothing outside the standard library is required.

    python docker/dev-postgres/generate_seed.py                 # 200 patients
    python docker/dev-postgres/generate_seed.py --patients 60   # small
    python docker/dev-postgres/generate_seed.py --seed 12345 --out /tmp/seed.sql

The generator models a hospital rather than sampling columns independently:
staff have roles that constrain what they can do, patients carry a condition
profile that drives their complaints, drugs, labs and imaging, bills are summed
from the items actually raised, and every timestamp respects the one before it.
"""

from __future__ import annotations

import argparse
import datetime as dt
import random
import uuid
from pathlib import Path

# ---------------------------------------------------------------------------
# Time window
# ---------------------------------------------------------------------------

# "Today" for the generated hospital. Data runs up to here and appointments run
# a few weeks past it, so "who is booked next week" and "admitted right now"
# both have real answers.
TODAY = dt.date(2026, 9, 4)
HISTORY_START = dt.date(2023, 1, 2)
FUTURE_END = TODAY + dt.timedelta(days=45)

# ---------------------------------------------------------------------------
# Reference data
# ---------------------------------------------------------------------------

COUNTIES = [
    ("Nairobi", "Nairobi"), ("Kiambu", "Central"), ("Nakuru", "Rift Valley"),
    ("Mombasa", "Coast"), ("Kisumu", "Nyanza"), ("Uasin Gishu", "Rift Valley"),
    ("Machakos", "Eastern"), ("Kakamega", "Western"), ("Meru", "Eastern"),
    ("Nyeri", "Central"), ("Kilifi", "Coast"), ("Bungoma", "Western"),
    ("Kericho", "Rift Valley"), ("Embu", "Eastern"), ("Garissa", "North Eastern"),
    ("Homa Bay", "Nyanza"), ("Murang'a", "Central"), ("Siaya", "Nyanza"),
    ("Kajiado", "Rift Valley"), ("Trans Nzoia", "Rift Valley"),
]

SUB_COUNTIES = ["Central", "East", "West", "North", "South", "Township", "Municipality"]

FIRST_M = ["John", "Peter", "James", "David", "Samuel", "Daniel", "Joseph", "Michael",
           "Brian", "Kevin", "Dennis", "Collins", "Victor", "Elijah", "Isaac", "Patrick",
           "Anthony", "Stephen", "Moses", "Fredrick", "George", "Vincent", "Charles", "Simon"]
FIRST_F = ["Mary", "Grace", "Faith", "Jane", "Esther", "Ann", "Caroline", "Mercy",
           "Sarah", "Joyce", "Lucy", "Rose", "Nancy", "Beatrice", "Purity", "Elizabeth",
           "Catherine", "Susan", "Winnie", "Doris", "Agnes", "Hellen", "Pauline", "Rita"]
SURNAMES = ["Kamau", "Otieno", "Wanjiru", "Mwangi", "Achieng", "Kiprop", "Njoroge",
            "Omondi", "Chebet", "Muthoni", "Barasa", "Wekesa", "Kipchoge", "Nyambura",
            "Ochieng", "Mutua", "Kimani", "Wairimu", "Simiyu", "Odhiambo", "Cheruiyot",
            "Maina", "Atieno", "Musyoka", "Korir", "Adhiambo", "Gitonga", "Naliaka",
            "Kariuki", "Wafula", "Njeri", "Osoro", "Mbugua", "Chelangat"]

OCCUPATIONS = ["Farmer", "Teacher", "Trader", "Boda boda rider", "Nurse", "Accountant",
               "Mechanic", "Tailor", "Shopkeeper", "Driver", "Student", "Casual labourer",
               "Civil servant", "Hairdresser", "Carpenter", "Security guard", "Unemployed",
               "Retired", "Chef", "Electrician", "Housewife", "Fisherman", "Clerk"]
EDUCATION = ["None", "Primary", "Secondary", "Certificate", "Diploma", "Degree", "Postgraduate"]
MARITAL = ["single", "married", "divorced", "widowed", "separated"]
LANGUAGES = ["English", "Swahili", "Kikuyu", "Luo", "Kalenjin", "Luhya", "Kamba", "Somali"]
BLOOD = ["O+", "O-", "A+", "A-", "B+", "B-", "AB+", "AB-"]
BLOOD_W = [38, 5, 24, 3, 18, 2, 8, 2]

# (code, name, dept_type, floor, has_ward)
DEPARTMENTS = [
    ("EMERG", "Emergency", "clinical", "Ground", True),
    ("GENMED", "General Medicine", "clinical", "1st", True),
    ("SURG", "Surgery", "clinical", "2nd", True),
    ("PAEDS", "Paediatrics", "clinical", "1st", True),
    ("OBGYN", "Obstetrics & Gynaecology", "clinical", "2nd", True),
    ("CARD", "Cardiology", "clinical", "3rd", False),
    ("ORTHO", "Orthopaedics", "clinical", "2nd", True),
    ("ONCOL", "Oncology", "clinical", "3rd", False),
    ("DERM", "Dermatology", "clinical", "1st", False),
    ("ENT", "ENT", "clinical", "1st", False),
    ("OPHTH", "Ophthalmology", "clinical", "1st", False),
    ("PSYCH", "Psychiatry", "clinical", "4th", True),
    ("RENAL", "Renal", "clinical", "3rd", True),
    ("ICU", "Critical Care", "clinical", "4th", True),
    ("LAB", "Laboratory", "diagnostic", "Ground", False),
    ("RAD", "Radiology", "diagnostic", "Ground", False),
    ("PHARM", "Pharmacy", "support", "Ground", False),
    ("RECORDS", "Health Records", "administrative", "Ground", False),
]

# (code, name, dept_code, ward_type, capacity)
WARDS = [
    ("MWA", "Medical Ward A", "GENMED", "general", 24),
    ("MWB", "Medical Ward B", "GENMED", "general", 24),
    ("SW", "Surgical Ward", "SURG", "surgical", 20),
    ("PW", "Paediatric Ward", "PAEDS", "paediatric", 18),
    ("MAT", "Maternity Ward", "OBGYN", "maternity", 16),
    ("LAB_W", "Labour Ward", "OBGYN", "maternity", 8),
    ("ICU_W", "Intensive Care Unit", "ICU", "icu", 8),
    ("HDU", "High Dependency Unit", "ICU", "hdu", 6),
    ("ISO", "Isolation Ward", "EMERG", "isolation", 10),
    ("ORTHW", "Orthopaedic Ward", "ORTHO", "surgical", 14),
    ("RENW", "Renal Unit", "RENAL", "renal", 8),
    ("PSYW", "Psychiatric Ward", "PSYCH", "psychiatric", 12),
]

# Doctor specialties mapped to their department code.
DOCTOR_SPECIALTIES = [
    ("Emergency Medicine", "EMERG", 3), ("Internal Medicine", "GENMED", 4),
    ("General Surgery", "SURG", 3), ("Paediatrics", "PAEDS", 3),
    ("Obstetrics & Gynaecology", "OBGYN", 3), ("Cardiology", "CARD", 2),
    ("Orthopaedic Surgery", "ORTHO", 2), ("Oncology", "ONCOL", 2),
    ("Dermatology", "DERM", 1), ("Otorhinolaryngology", "ENT", 1),
    ("Ophthalmology", "OPHTH", 1), ("Psychiatry", "PSYCH", 1),
    ("Nephrology", "RENAL", 2), ("Anaesthesiology", "ICU", 2),
    ("Family Medicine", "GENMED", 2), ("Radiology", "RAD", 2),
]

ICD10 = [
    ("B50.9", "Plasmodium falciparum malaria, unspecified", "Infectious", "Certain infectious and parasitic diseases", True),
    ("A09", "Infectious gastroenteritis and colitis", "Infectious", "Certain infectious and parasitic diseases", False),
    ("A01.0", "Typhoid fever", "Infectious", "Certain infectious and parasitic diseases", True),
    ("A15.0", "Tuberculosis of lung, confirmed", "Infectious", "Certain infectious and parasitic diseases", True),
    ("B20", "HIV disease", "Infectious", "Certain infectious and parasitic diseases", True),
    ("J18.9", "Pneumonia, unspecified organism", "Respiratory", "Diseases of the respiratory system", False),
    ("J45.9", "Asthma, unspecified", "Respiratory", "Diseases of the respiratory system", False),
    ("J44.9", "Chronic obstructive pulmonary disease", "Respiratory", "Diseases of the respiratory system", False),
    ("J06.9", "Acute upper respiratory infection", "Respiratory", "Diseases of the respiratory system", False),
    ("J03.9", "Acute tonsillitis, unspecified", "Respiratory", "Diseases of the respiratory system", False),
    ("E11.9", "Type 2 diabetes mellitus without complications", "Endocrine", "Endocrine, nutritional and metabolic diseases", False),
    ("E11.2", "Type 2 diabetes with kidney complications", "Endocrine", "Endocrine, nutritional and metabolic diseases", False),
    ("E78.5", "Hyperlipidaemia, unspecified", "Endocrine", "Endocrine, nutritional and metabolic diseases", False),
    ("E66.9", "Obesity, unspecified", "Endocrine", "Endocrine, nutritional and metabolic diseases", False),
    ("I10", "Essential (primary) hypertension", "Circulatory", "Diseases of the circulatory system", False),
    ("I25.1", "Atherosclerotic heart disease", "Circulatory", "Diseases of the circulatory system", False),
    ("I50.9", "Heart failure, unspecified", "Circulatory", "Diseases of the circulatory system", False),
    ("I63.9", "Cerebral infarction, unspecified", "Circulatory", "Diseases of the circulatory system", False),
    ("N18.3", "Chronic kidney disease, stage 3", "Genitourinary", "Diseases of the genitourinary system", False),
    ("N18.5", "Chronic kidney disease, stage 5", "Genitourinary", "Diseases of the genitourinary system", False),
    ("N39.0", "Urinary tract infection, site not specified", "Genitourinary", "Diseases of the genitourinary system", False),
    ("K21.0", "Gastro-oesophageal reflux disease", "Digestive", "Diseases of the digestive system", False),
    ("K29.7", "Gastritis, unspecified", "Digestive", "Diseases of the digestive system", False),
    ("K35.8", "Acute appendicitis, unspecified", "Digestive", "Diseases of the digestive system", False),
    ("K40.9", "Inguinal hernia without obstruction", "Digestive", "Diseases of the digestive system", False),
    ("D50.9", "Iron deficiency anaemia, unspecified", "Blood", "Diseases of the blood", False),
    ("D57.1", "Sickle-cell disease without crisis", "Blood", "Diseases of the blood", False),
    ("G43.9", "Migraine, unspecified", "Nervous", "Diseases of the nervous system", False),
    ("G40.9", "Epilepsy, unspecified", "Nervous", "Diseases of the nervous system", False),
    ("F32.1", "Moderate depressive episode", "Mental", "Mental and behavioural disorders", False),
    ("F41.1", "Generalised anxiety disorder", "Mental", "Mental and behavioural disorders", False),
    ("L30.9", "Dermatitis, unspecified", "Skin", "Diseases of the skin", False),
    ("L03.9", "Cellulitis, unspecified", "Skin", "Diseases of the skin", False),
    ("M54.5", "Low back pain", "Musculoskeletal", "Diseases of the musculoskeletal system", False),
    ("M17.9", "Osteoarthritis of knee, unspecified", "Musculoskeletal", "Diseases of the musculoskeletal system", False),
    ("S52.5", "Fracture of lower end of radius", "Injury", "Injury and poisoning", False),
    ("S82.6", "Fracture of lateral malleolus", "Injury", "Injury and poisoning", False),
    ("O80", "Single spontaneous delivery", "Pregnancy", "Pregnancy, childbirth and the puerperium", False),
    ("O82", "Delivery by caesarean section", "Pregnancy", "Pregnancy, childbirth and the puerperium", False),
    ("O14.9", "Pre-eclampsia, unspecified", "Pregnancy", "Pregnancy, childbirth and the puerperium", False),
    ("H25.9", "Age-related cataract, unspecified", "Eye", "Diseases of the eye", False),
    ("H66.9", "Otitis media, unspecified", "Ear", "Diseases of the ear", False),
    ("A41.9", "Sepsis, unspecified organism", "Infectious", "Certain infectious and parasitic diseases", False),
    ("C50.9", "Malignant neoplasm of breast, unspecified", "Neoplasm", "Neoplasms", False),
    ("C34.9", "Malignant neoplasm of bronchus or lung", "Neoplasm", "Neoplasms", False),
]

# Clinical profiles: what a patient with this condition actually generates.
# (icd10, dept_code, complaints, meds(generic), lab panels, imaging(modality,body_part) or None,
#  chronic, admit_rate, severity)
CONDITIONS = [
    ("B50.9", "GENMED", ["Fever and chills", "Fever and body weakness", "Headache and vomiting"],
     ["Artemether-Lumefantrine", "Paracetamol"], ["Malaria RDT", "Complete Blood Count"], None, False, 0.20, "acute"),
    ("A09", "GENMED", ["Diarrhoea and vomiting", "Abdominal cramps", "Loose stools for 3 days"],
     ["Oral Rehydration Salts", "Metronidazole"], ["Stool Analysis", "Urea & Electrolytes"], None, False, 0.15, "acute"),
    ("A01.0", "GENMED", ["Prolonged fever", "Abdominal pain and fever"],
     ["Ciprofloxacin", "Paracetamol"], ["Widal Test", "Complete Blood Count"], None, False, 0.30, "acute"),
    ("A15.0", "GENMED", ["Persistent cough for 3 weeks", "Night sweats and weight loss"],
     ["Rifampicin-Isoniazid", "Pyridoxine"], ["Sputum GeneXpert", "Complete Blood Count"], ("X-Ray", "Chest"), True, 0.35, "chronic"),
    ("B20", "GENMED", ["Routine HIV review", "Recurrent infections", "Weight loss"],
     ["Dolutegravir-TLD", "Cotrimoxazole"], ["HIV Viral Load", "CD4 Count", "Complete Blood Count"], None, True, 0.10, "chronic"),
    ("J18.9", "GENMED", ["Cough with fever", "Difficulty breathing", "Chest pain on breathing"],
     ["Amoxicillin-Clavulanate", "Paracetamol"], ["Complete Blood Count", "C-Reactive Protein"], ("X-Ray", "Chest"), False, 0.45, "acute"),
    ("J45.9", "GENMED", ["Difficulty breathing", "Wheezing and cough", "Chest tightness at night"],
     ["Salbutamol", "Beclomethasone"], ["Complete Blood Count"], None, True, 0.20, "chronic"),
    ("J44.9", "GENMED", ["Chronic cough with sputum", "Breathlessness on exertion"],
     ["Salbutamol", "Prednisolone"], ["Complete Blood Count"], ("X-Ray", "Chest"), True, 0.30, "chronic"),
    ("J06.9", "GENMED", ["Sore throat and runny nose", "Blocked nose and cough"],
     ["Paracetamol", "Cetirizine"], [], None, False, 0.02, "minor"),
    ("J03.9", "ENT", ["Sore throat and difficulty swallowing", "Painful swallowing"],
     ["Amoxicillin", "Ibuprofen"], ["Complete Blood Count"], None, False, 0.05, "minor"),
    ("E11.9", "GENMED", ["Diabetes review", "Increased thirst and urination", "Fatigue"],
     ["Metformin", "Glibenclamide"], ["HbA1c", "Fasting Blood Glucose", "Lipid Profile"], None, True, 0.12, "chronic"),
    ("E11.2", "RENAL", ["Diabetes review with swelling", "Leg swelling and fatigue"],
     ["Metformin", "Lisinopril"], ["HbA1c", "Renal Function Test", "Urinalysis"], None, True, 0.25, "chronic"),
    ("E78.5", "CARD", ["Lipid clinic review", "Routine health check"],
     ["Atorvastatin"], ["Lipid Profile", "Liver Function Test"], None, True, 0.03, "chronic"),
    ("E66.9", "GENMED", ["Weight management review", "Difficulty losing weight"],
     ["Metformin"], ["Lipid Profile", "HbA1c"], None, True, 0.02, "chronic"),
    ("I10", "CARD", ["Hypertension review", "Headache and dizziness", "Blood pressure check"],
     ["Amlodipine", "Losartan", "Hydrochlorothiazide"], ["Renal Function Test", "Lipid Profile"], None, True, 0.08, "chronic"),
    ("I25.1", "CARD", ["Chest pain on exertion", "Chest tightness"],
     ["Aspirin", "Atorvastatin", "Bisoprolol"], ["Lipid Profile", "Cardiac Troponin"], ("Echocardiogram", "Heart"), True, 0.35, "chronic"),
    ("I50.9", "CARD", ["Swollen ankles and breathlessness", "Difficulty breathing when lying flat"],
     ["Furosemide", "Lisinopril", "Bisoprolol"], ["Renal Function Test", "Complete Blood Count"], ("Echocardiogram", "Heart"), True, 0.55, "chronic"),
    ("I63.9", "GENMED", ["Sudden weakness on one side", "Slurred speech"],
     ["Aspirin", "Atorvastatin"], ["Complete Blood Count", "Fasting Blood Glucose"], ("CT", "Brain"), True, 0.85, "severe"),
    ("N18.3", "RENAL", ["Renal clinic review", "Fatigue and reduced urine output"],
     ["Losartan", "Ferrous Sulphate"], ["Renal Function Test", "Urinalysis", "Complete Blood Count"], ("Ultrasound", "Renal"), True, 0.20, "chronic"),
    ("N18.5", "RENAL", ["Dialysis review", "Severe fatigue and swelling"],
     ["Erythropoietin", "Calcium Carbonate"], ["Renal Function Test", "Complete Blood Count"], ("Ultrasound", "Renal"), True, 0.60, "severe"),
    ("N39.0", "GENMED", ["Painful urination", "Urinary frequency", "Burning on urination"],
     ["Nitrofurantoin", "Paracetamol"], ["Urinalysis", "Urine Culture"], None, False, 0.08, "minor"),
    ("K21.0", "GENMED", ["Heartburn after meals", "Chest burning"],
     ["Omeprazole"], [], None, True, 0.02, "chronic"),
    ("K29.7", "GENMED", ["Epigastric pain", "Nausea and stomach pain"],
     ["Omeprazole", "Metoclopramide"], ["Complete Blood Count"], None, False, 0.05, "minor"),
    ("K35.8", "SURG", ["Right lower abdominal pain", "Severe abdominal pain and vomiting"],
     ["Ceftriaxone", "Metronidazole", "Diclofenac"], ["Complete Blood Count", "Urinalysis"], ("Ultrasound", "Abdomen"), False, 0.90, "surgical"),
    ("K40.9", "SURG", ["Groin swelling", "Lump in the groin"],
     ["Diclofenac", "Paracetamol"], ["Complete Blood Count"], ("Ultrasound", "Abdomen"), False, 0.70, "surgical"),
    ("D50.9", "GENMED", ["Fatigue and pallor", "Dizziness and tiredness"],
     ["Ferrous Sulphate", "Folic Acid"], ["Complete Blood Count", "Serum Ferritin"], None, False, 0.15, "chronic"),
    ("D57.1", "PAEDS", ["Bone pain crisis", "Joint pain and fever"],
     ["Folic Acid", "Paracetamol", "Penicillin V"], ["Complete Blood Count"], None, True, 0.40, "chronic"),
    ("G43.9", "GENMED", ["Severe headache with nausea", "Recurrent headaches"],
     ["Sumatriptan", "Ibuprofen"], [], None, True, 0.03, "chronic"),
    ("G40.9", "GENMED", ["Seizure episode", "Fits at home"],
     ["Carbamazepine", "Sodium Valproate"], ["Complete Blood Count", "Urea & Electrolytes"], ("CT", "Brain"), True, 0.35, "chronic"),
    ("F32.1", "PSYCH", ["Low mood and poor sleep", "Loss of interest and appetite"],
     ["Fluoxetine", "Amitriptyline"], ["Thyroid Function Test"], None, True, 0.15, "chronic"),
    ("F41.1", "PSYCH", ["Excessive worry", "Palpitations and anxiety"],
     ["Sertraline"], ["Thyroid Function Test"], None, True, 0.05, "chronic"),
    ("L30.9", "DERM", ["Itchy skin rash", "Skin rash on arms"],
     ["Hydrocortisone Cream", "Cetirizine"], [], None, False, 0.02, "minor"),
    ("L03.9", "DERM", ["Red painful swollen leg", "Spreading skin infection"],
     ["Flucloxacillin", "Ibuprofen"], ["Complete Blood Count", "C-Reactive Protein"], None, False, 0.35, "acute"),
    ("M54.5", "ORTHO", ["Lower back pain", "Back pain radiating to leg"],
     ["Diclofenac", "Paracetamol"], [], ("X-Ray", "Lumbar Spine"), True, 0.05, "chronic"),
    ("M17.9", "ORTHO", ["Knee pain on walking", "Painful stiff knees"],
     ["Diclofenac", "Paracetamol"], [], ("X-Ray", "Right Knee"), True, 0.10, "chronic"),
    ("S52.5", "ORTHO", ["Wrist injury after a fall", "Painful swollen wrist"],
     ["Diclofenac", "Paracetamol"], [], ("X-Ray", "Left Wrist"), False, 0.35, "surgical"),
    ("S82.6", "ORTHO", ["Ankle injury", "Cannot bear weight after a fall"],
     ["Diclofenac", "Paracetamol"], [], ("X-Ray", "Right Ankle"), False, 0.45, "surgical"),
    ("O80", "OBGYN", ["In labour", "Contractions since morning"],
     ["Oxytocin", "Paracetamol"], ["Complete Blood Count", "HIV Test"], None, False, 0.95, "maternity"),
    ("O82", "OBGYN", ["Failure to progress in labour", "Previous caesarean in labour"],
     ["Ceftriaxone", "Oxytocin", "Diclofenac"], ["Complete Blood Count", "Blood Grouping"], None, False, 0.98, "maternity"),
    ("O14.9", "OBGYN", ["Headache and swelling in pregnancy", "High blood pressure in pregnancy"],
     ["Methyldopa", "Nifedipine"], ["Urinalysis", "Liver Function Test", "Renal Function Test"], None, False, 0.75, "maternity"),
    ("H25.9", "OPHTH", ["Gradual loss of vision", "Cloudy vision"],
     ["Prednisolone Eye Drops"], ["Fasting Blood Glucose"], None, True, 0.30, "surgical"),
    ("H66.9", "ENT", ["Ear pain and discharge", "Ear pain in a child"],
     ["Amoxicillin", "Paracetamol"], [], None, False, 0.05, "minor"),
    ("A41.9", "ICU", ["Fever and confusion", "Collapse at home"],
     ["Ceftriaxone", "Metronidazole", "Paracetamol"], ["Complete Blood Count", "C-Reactive Protein", "Blood Culture", "Urea & Electrolytes"], None, False, 0.95, "severe"),
    ("C50.9", "ONCOL", ["Breast lump", "Breast lump and skin change"],
     ["Tamoxifen", "Morphine"], ["Complete Blood Count", "Liver Function Test"], ("Mammography", "Both Breasts"), True, 0.40, "chronic"),
    ("C34.9", "ONCOL", ["Cough with blood", "Weight loss and cough"],
     ["Morphine", "Dexamethasone"], ["Complete Blood Count", "Liver Function Test"], ("CT", "Chest"), True, 0.55, "severe"),
]

# (generic, brand, class, atc, form, strength, unit, route, price, controlled)
MEDICATIONS = [
    ("Paracetamol", "Panadol", "Analgesic", "N02BE01", "tablet", "500mg", "mg", "oral", 5, False),
    ("Ibuprofen", "Brufen", "NSAID", "M01AE01", "tablet", "400mg", "mg", "oral", 8, False),
    ("Diclofenac", "Voltaren", "NSAID", "M01AB05", "tablet", "50mg", "mg", "oral", 10, False),
    ("Amoxicillin", "Amoxil", "Antibiotic", "J01CA04", "capsule", "500mg", "mg", "oral", 15, False),
    ("Amoxicillin-Clavulanate", "Augmentin", "Antibiotic", "J01CR02", "tablet", "625mg", "mg", "oral", 45, False),
    ("Ceftriaxone", "Rocephin", "Antibiotic", "J01DD04", "injection", "1g", "g", "intravenous", 180, False),
    ("Metronidazole", "Flagyl", "Antibiotic", "J01XD01", "tablet", "400mg", "mg", "oral", 8, False),
    ("Ciprofloxacin", "Cipro", "Antibiotic", "J01MA02", "tablet", "500mg", "mg", "oral", 20, False),
    ("Nitrofurantoin", "Macrodantin", "Antibiotic", "J01XE01", "capsule", "100mg", "mg", "oral", 18, False),
    ("Flucloxacillin", "Floxapen", "Antibiotic", "J01CF05", "capsule", "500mg", "mg", "oral", 25, False),
    ("Cotrimoxazole", "Septrin", "Antibiotic", "J01EE01", "tablet", "960mg", "mg", "oral", 6, False),
    ("Penicillin V", "Pen-V", "Antibiotic", "J01CE02", "tablet", "250mg", "mg", "oral", 7, False),
    ("Artemether-Lumefantrine", "Coartem", "Antimalarial", "P01BF01", "tablet", "20/120mg", "mg", "oral", 120, False),
    ("Rifampicin-Isoniazid", "Rifinah", "Antitubercular", "J04AM02", "tablet", "150/75mg", "mg", "oral", 90, False),
    ("Pyridoxine", "Vitamin B6", "Vitamin", "A11HA02", "tablet", "50mg", "mg", "oral", 4, False),
    ("Dolutegravir-TLD", "TLD", "Antiretroviral", "J05AR27", "tablet", "50/300/300mg", "mg", "oral", 0, False),
    ("Metformin", "Glucophage", "Antidiabetic", "A10BA02", "tablet", "500mg", "mg", "oral", 12, False),
    ("Glibenclamide", "Daonil", "Antidiabetic", "A10BB01", "tablet", "5mg", "mg", "oral", 9, False),
    ("Amlodipine", "Norvasc", "Calcium channel blocker", "C08CA01", "tablet", "5mg", "mg", "oral", 14, False),
    ("Losartan", "Cozaar", "ARB", "C09CA01", "tablet", "50mg", "mg", "oral", 22, False),
    ("Lisinopril", "Zestril", "ACE inhibitor", "C09AA03", "tablet", "10mg", "mg", "oral", 16, False),
    ("Hydrochlorothiazide", "Hydrosaluric", "Diuretic", "C03AA03", "tablet", "25mg", "mg", "oral", 6, False),
    ("Furosemide", "Lasix", "Loop diuretic", "C03CA01", "tablet", "40mg", "mg", "oral", 7, False),
    ("Bisoprolol", "Concor", "Beta-blocker", "C07AB07", "tablet", "5mg", "mg", "oral", 25, False),
    ("Atorvastatin", "Lipitor", "Statin", "C10AA05", "tablet", "20mg", "mg", "oral", 30, False),
    ("Aspirin", "Cardioprin", "Antiplatelet", "B01AC06", "tablet", "75mg", "mg", "oral", 4, False),
    ("Salbutamol", "Ventolin", "Bronchodilator", "R03AC02", "inhaler", "100mcg", "mcg", "inhaled", 350, False),
    ("Beclomethasone", "Becotide", "Inhaled corticosteroid", "R03BA01", "inhaler", "200mcg", "mcg", "inhaled", 480, False),
    ("Prednisolone", "Prednol", "Corticosteroid", "H02AB06", "tablet", "5mg", "mg", "oral", 5, False),
    ("Dexamethasone", "Decadron", "Corticosteroid", "H02AB02", "injection", "4mg", "mg", "intravenous", 60, False),
    ("Omeprazole", "Losec", "PPI", "A02BC01", "capsule", "20mg", "mg", "oral", 15, False),
    ("Metoclopramide", "Plasil", "Antiemetic", "A03FA01", "tablet", "10mg", "mg", "oral", 6, False),
    ("Cetirizine", "Zyrtec", "Antihistamine", "R06AE07", "tablet", "10mg", "mg", "oral", 8, False),
    ("Hydrocortisone Cream", "Cortaid", "Topical steroid", "D07AA02", "cream", "1%", "%", "topical", 120, False),
    ("Ferrous Sulphate", "FeSO4", "Iron supplement", "B03AA07", "tablet", "200mg", "mg", "oral", 4, False),
    ("Folic Acid", "Folvite", "Vitamin", "B03BB01", "tablet", "5mg", "mg", "oral", 3, False),
    ("Oral Rehydration Salts", "ORS", "Electrolyte", "A07CA", "powder", "20.5g", "g", "oral", 25, False),
    ("Morphine", "MST", "Opioid analgesic", "N02AA01", "injection", "10mg", "mg", "intravenous", 90, True),
    ("Carbamazepine", "Tegretol", "Anticonvulsant", "N03AF01", "tablet", "200mg", "mg", "oral", 18, False),
    ("Sodium Valproate", "Epilim", "Anticonvulsant", "N03AG01", "tablet", "200mg", "mg", "oral", 24, False),
    ("Fluoxetine", "Prozac", "SSRI", "N06AB03", "capsule", "20mg", "mg", "oral", 28, False),
    ("Sertraline", "Zoloft", "SSRI", "N06AB06", "tablet", "50mg", "mg", "oral", 34, False),
    ("Amitriptyline", "Tryptizol", "TCA", "N06AA09", "tablet", "25mg", "mg", "oral", 10, True),
    ("Sumatriptan", "Imigran", "Triptan", "N02CC01", "tablet", "50mg", "mg", "oral", 220, False),
    ("Methyldopa", "Aldomet", "Antihypertensive", "C02AB01", "tablet", "250mg", "mg", "oral", 12, False),
    ("Nifedipine", "Adalat", "Calcium channel blocker", "C08CA05", "tablet", "20mg", "mg", "oral", 15, False),
    ("Oxytocin", "Syntocinon", "Uterotonic", "H01BB02", "injection", "10IU", "IU", "intravenous", 70, False),
    ("Tamoxifen", "Nolvadex", "Antineoplastic", "L02BA01", "tablet", "20mg", "mg", "oral", 65, False),
    ("Erythropoietin", "Eprex", "Haematopoietic", "B03XA01", "injection", "4000IU", "IU", "subcutaneous", 2400, False),
    ("Calcium Carbonate", "Calcichew", "Mineral supplement", "A12AA04", "tablet", "500mg", "mg", "oral", 6, False),
    ("Prednisolone Eye Drops", "Pred Forte", "Ophthalmic steroid", "S01BA04", "drops", "1%", "%", "ophthalmic", 380, False),
]

# (panel, test, specimen, unit, ref_low, ref_high, ref_text, tat_hours, price)
LAB_TESTS = [
    ("Complete Blood Count", "Haemoglobin", "Whole blood", "g/dL", 12.0, 16.0, None, 2, 600),
    ("Complete Blood Count", "White Cell Count", "Whole blood", "x10^9/L", 4.0, 11.0, None, 2, 0),
    ("Complete Blood Count", "Platelets", "Whole blood", "x10^9/L", 150, 400, None, 2, 0),
    ("Complete Blood Count", "Haematocrit", "Whole blood", "%", 36.0, 48.0, None, 2, 0),
    ("Renal Function Test", "Urea", "Serum", "mmol/L", 2.5, 7.8, None, 4, 900),
    ("Renal Function Test", "Creatinine", "Serum", "umol/L", 60, 110, None, 4, 0),
    ("Renal Function Test", "eGFR", "Serum", "mL/min", 90, 140, None, 4, 0),
    ("Renal Function Test", "Potassium", "Serum", "mmol/L", 3.5, 5.1, None, 4, 0),
    ("Urea & Electrolytes", "Sodium", "Serum", "mmol/L", 135, 145, None, 4, 750),
    ("Urea & Electrolytes", "Potassium", "Serum", "mmol/L", 3.5, 5.1, None, 4, 0),
    ("Urea & Electrolytes", "Chloride", "Serum", "mmol/L", 98, 107, None, 4, 0),
    ("Liver Function Test", "ALT", "Serum", "U/L", 7, 45, None, 4, 1100),
    ("Liver Function Test", "AST", "Serum", "U/L", 8, 40, None, 4, 0),
    ("Liver Function Test", "Total Bilirubin", "Serum", "umol/L", 3, 21, None, 4, 0),
    ("Liver Function Test", "Albumin", "Serum", "g/L", 35, 50, None, 4, 0),
    ("Lipid Profile", "Total Cholesterol", "Serum", "mmol/L", 3.0, 5.2, None, 6, 1400),
    ("Lipid Profile", "LDL Cholesterol", "Serum", "mmol/L", 1.0, 3.4, None, 6, 0),
    ("Lipid Profile", "HDL Cholesterol", "Serum", "mmol/L", 1.0, 2.2, None, 6, 0),
    ("Lipid Profile", "Triglycerides", "Serum", "mmol/L", 0.5, 1.7, None, 6, 0),
    ("HbA1c", "HbA1c", "Whole blood", "%", 4.0, 6.0, None, 24, 1800),
    ("Fasting Blood Glucose", "Fasting Glucose", "Plasma", "mmol/L", 3.9, 5.5, None, 1, 350),
    ("Malaria RDT", "Malaria Antigen", "Whole blood", None, None, None, "Negative", 1, 300),
    ("HIV Test", "HIV 1/2 Antibody", "Serum", None, None, None, "Non-reactive", 2, 0),
    ("HIV Viral Load", "HIV RNA", "Plasma", "copies/mL", 0, 50, None, 72, 3500),
    ("CD4 Count", "CD4 Absolute", "Whole blood", "cells/uL", 500, 1500, None, 24, 2200),
    ("Urinalysis", "Urine Protein", "Urine", None, None, None, "Negative", 2, 400),
    ("Urinalysis", "Urine Glucose", "Urine", None, None, None, "Negative", 2, 0),
    ("Urinalysis", "Urine Leucocytes", "Urine", None, None, None, "Negative", 2, 0),
    ("Urine Culture", "Urine Culture", "Urine", None, None, None, "No growth", 48, 1600),
    ("Blood Culture", "Blood Culture", "Whole blood", None, None, None, "No growth", 72, 2400),
    ("Stool Analysis", "Stool Microscopy", "Stool", None, None, None, "No ova or cysts", 4, 500),
    ("Widal Test", "Widal Antigen O", "Serum", None, None, None, "Negative", 6, 700),
    ("Sputum GeneXpert", "MTB Detection", "Sputum", None, None, None, "Not detected", 24, 0),
    ("C-Reactive Protein", "CRP", "Serum", "mg/L", 0, 5, None, 4, 1200),
    ("Thyroid Function Test", "TSH", "Serum", "mIU/L", 0.4, 4.0, None, 24, 2000),
    ("Thyroid Function Test", "Free T4", "Serum", "pmol/L", 9.0, 25.0, None, 24, 0),
    ("Serum Ferritin", "Ferritin", "Serum", "ug/L", 30, 300, None, 24, 2600),
    ("Cardiac Troponin", "Troponin I", "Serum", "ng/L", 0, 14, None, 2, 3200),
    ("Blood Grouping", "ABO Group", "Whole blood", None, None, None, "Reported", 2, 500),
]

VACCINES = [
    ("BCG", "Tuberculosis", 1, "intradermal", True),
    ("OPV", "Poliomyelitis", 4, "oral", True),
    ("Pentavalent", "DPT-HepB-Hib", 3, "intramuscular", True),
    ("Pneumococcal (PCV10)", "Pneumococcal disease", 3, "intramuscular", True),
    ("Rotavirus", "Rotavirus gastroenteritis", 2, "oral", True),
    ("Measles-Rubella", "Measles and rubella", 2, "subcutaneous", True),
    ("Yellow Fever", "Yellow fever", 1, "subcutaneous", False),
    ("Tetanus Toxoid", "Tetanus", 5, "intramuscular", True),
    ("COVID-19 (Astra)", "COVID-19", 2, "intramuscular", False),
    ("Hepatitis B (adult)", "Hepatitis B", 3, "intramuscular", False),
    ("Influenza", "Seasonal influenza", 1, "intramuscular", False),
    ("HPV", "Human papillomavirus", 2, "intramuscular", True),
]

ALLERGENS = [
    ("Penicillin", "Antibiotic", "Cephalosporins"), ("Sulfonamides", "Antibiotic", None),
    ("Aspirin", "NSAID", "Other NSAIDs"), ("Ibuprofen", "NSAID", "Other NSAIDs"),
    ("Codeine", "Opioid", "Morphine"), ("Iodine contrast", "Contrast medium", "Povidone-iodine"),
    ("Latex", "Environmental", "Banana, avocado"), ("Peanuts", "Food", "Tree nuts"),
    ("Shellfish", "Food", "Iodine contrast"), ("Eggs", "Food", None),
    ("Cow's milk", "Food", None), ("Pollen", "Environmental", None),
    ("Dust mites", "Environmental", None), ("Bee venom", "Insect", "Wasp venom"),
    ("Sulphur drugs", "Antibiotic", None), ("Carbamazepine", "Anticonvulsant", None),
    ("Metformin", "Antidiabetic", None), ("Erythromycin", "Antibiotic", "Azithromycin"),
]

REACTIONS = ["Urticaria", "Angioedema", "Anaphylaxis", "Maculopapular rash", "Bronchospasm",
             "Nausea and vomiting", "Diarrhoea", "Swollen lips", "Itching", "Wheezing"]

INSURERS = [
    ("Social Health Authority", "SHA", "nhif"), ("Britam Health", "Britam", "private"),
    ("Jubilee Health Insurance", "Jubilee", "private"), ("AAR Insurance", "AAR", "private"),
    ("CIC Insurance Group", "CIC", "private"), ("Madison Insurance", "Madison", "private"),
    ("APA Insurance", "APA", "private"), ("Linda Jamii", "Linda", "micro"),
    ("Old Mutual Health", "OldMutual", "corporate"), ("Sanlam Health", "Sanlam", "corporate"),
]

# (code, description, category, is_surgical, minutes)
PROCEDURE_CODES = [
    ("PRC001", "Intravenous cannulation", "Nursing", False, 10),
    ("PRC002", "Urethral catheterisation", "Nursing", False, 15),
    ("PRC003", "Wound dressing", "Nursing", False, 20),
    ("PRC004", "Suturing of laceration", "Minor surgery", False, 30),
    ("PRC005", "Incision and drainage of abscess", "Minor surgery", False, 35),
    ("PRC006", "Plaster of Paris cast application", "Orthopaedics", False, 40),
    ("PRC007", "Nebulisation", "Respiratory", False, 20),
    ("PRC008", "Blood transfusion", "Haematology", False, 180),
    ("PRC009", "Lumbar puncture", "Diagnostic", False, 30),
    ("PRC010", "Haemodialysis session", "Renal", False, 240),
    ("PRC011", "Appendicectomy", "General surgery", True, 60),
    ("PRC012", "Inguinal hernia repair", "General surgery", True, 75),
    ("PRC013", "Caesarean section", "Obstetrics", True, 55),
    ("PRC014", "Exploratory laparotomy", "General surgery", True, 120),
    ("PRC015", "Open reduction and internal fixation", "Orthopaedics", True, 110),
    ("PRC016", "Closed reduction of fracture", "Orthopaedics", True, 45),
    ("PRC017", "Cataract extraction with IOL", "Ophthalmology", True, 40),
    ("PRC018", "Tonsillectomy", "ENT", True, 45),
    ("PRC019", "Cholecystectomy", "General surgery", True, 90),
    ("PRC020", "Mastectomy", "General surgery", True, 150),
    ("PRC021", "Debridement of wound", "General surgery", True, 50),
    ("PRC022", "Manual vacuum aspiration", "Obstetrics", True, 30),
    ("PRC023", "Skin biopsy", "Dermatology", False, 25),
    ("PRC024", "Upper GI endoscopy", "Diagnostic", False, 35),
    ("PRC025", "Physiotherapy session", "Rehabilitation", False, 45),
]


# ---------------------------------------------------------------------------
# SQL helpers
# ---------------------------------------------------------------------------

def q(value) -> str:
    """Render a Python value as a SQL literal."""
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        return "TRUE" if value else "FALSE"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, dt.datetime):
        return "'" + value.strftime("%Y-%m-%d %H:%M:%S%z") + "'"
    if isinstance(value, dt.date):
        return "'" + value.isoformat() + "'"
    return "'" + str(value).replace("'", "''") + "'"


class Writer:
    """Accumulates multi-row INSERT batches."""

    def __init__(self) -> None:
        self.parts: list[str] = []
        self.counts: dict[str, int] = {}

    def comment(self, text: str) -> None:
        self.parts.append(f"\n-- {text}\n")

    def insert(self, table: str, columns: list[str], rows: list[tuple]) -> None:
        if not rows:
            return
        self.counts[table] = self.counts.get(table, 0) + len(rows)
        collist = ",".join(columns)
        for start in range(0, len(rows), 100):
            chunk = rows[start:start + 100]
            values = ",\n  ".join("(" + ",".join(q(v) for v in row) + ")" for row in chunk)
            self.parts.append(f"INSERT INTO {table} ({collist}) VALUES\n  {values};\n")

    def render(self) -> str:
        return "".join(self.parts)


def tz(moment: dt.datetime) -> dt.datetime:
    return moment.replace(tzinfo=dt.timezone(dt.timedelta(hours=3)))  # EAT


def at(day: dt.date, hour: int, minute: int = 0) -> dt.datetime:
    return tz(dt.datetime(day.year, day.month, day.day, hour, minute))


# ---------------------------------------------------------------------------
# Generation
# ---------------------------------------------------------------------------

class Hospital:
    def __init__(self, rng: random.Random, n_patients: int) -> None:
        self.rng = rng
        self.n_patients = n_patients
        self.w = Writer()
        self.uuids: set[str] = set()

    def uid(self) -> str:
        while True:
            candidate = str(uuid.UUID(int=self.rng.getrandbits(128), version=4))
            if candidate not in self.uuids:
                self.uuids.add(candidate)
                return candidate

    def pick(self, seq):
        return self.rng.choice(seq)

    def chance(self, p: float) -> bool:
        return self.rng.random() < p

    # -- reference -----------------------------------------------------------

    def gen_reference(self) -> None:
        w, rng = self.w, self.rng

        w.comment("Counties")
        w.insert("counties", ["id", "name", "region"],
                 [(i + 1, n, r) for i, (n, r) in enumerate(COUNTIES)])

        w.comment("ICD-10 diagnosis codes")
        w.insert("icd10_codes", ["code", "description", "category", "chapter", "is_notifiable"],
                 [(c, d, cat, ch, notif) for c, d, cat, ch, notif in ICD10])

        w.comment("Procedure codes")
        w.insert("procedure_codes", ["code", "description", "category", "is_surgical", "typical_minutes"],
                 list(PROCEDURE_CODES))

        w.comment("Departments")
        self.dept_id = {}
        rows = []
        for i, (code, name, dtype, floor, _) in enumerate(DEPARTMENTS, start=1):
            self.dept_id[code] = i
            rows.append((i, code, name, dtype, floor, f"2{i:02d}", f"CC-{code}", True))
        w.insert("departments", ["id", "code", "name", "dept_type", "floor", "phone_ext", "cost_centre", "is_active"], rows)

        w.comment("Wards")
        self.ward_id = {}
        rows = []
        for i, (code, name, dept, wtype, cap) in enumerate(WARDS, start=1):
            self.ward_id[code] = i
            floor = next(d[3] for d in DEPARTMENTS if d[0] == dept)
            rows.append((i, code, name, self.dept_id[dept], wtype, floor, cap, True))
        w.insert("wards", ["id", "code", "name", "department_id", "ward_type", "floor", "bed_capacity", "is_active"], rows)

        w.comment("Beds")
        self.beds_by_ward: dict[int, list[int]] = {}
        rows, bed_id = [], 0
        for code, name, dept, wtype, cap in WARDS:
            wid = self.ward_id[code]
            self.beds_by_ward[wid] = []
            bed_type = {"icu": "icu", "hdu": "icu", "isolation": "isolation",
                        "paediatric": "cot", "maternity": "delivery"}.get(wtype, "standard")
            for n in range(1, cap + 1):
                bed_id += 1
                self.beds_by_ward[wid].append(bed_id)
                rows.append((bed_id, wid, f"{code}-{n:02d}", bed_type, "available", True))
        self.total_beds = bed_id
        w.insert("beds", ["id", "ward_id", "bed_no", "bed_type", "status", "is_active"], rows)

        w.comment("Medication formulary")
        self.med_id = {}
        rows = []
        for i, (gen, brand, cls, atc, form, strength, unit, route, price, controlled) in enumerate(MEDICATIONS, start=1):
            self.med_id[gen] = i
            rows.append((i, gen, brand, cls, atc, form, strength, unit, route, price, controlled, True, True))
        w.insert("medication_catalog",
                 ["id", "generic_name", "brand_name", "drug_class", "atc_code", "form", "strength",
                  "unit", "route_default", "unit_price_kes", "is_controlled", "is_formulary", "is_active"], rows)

        w.comment("Drug interactions")
        pairs = [
            ("Aspirin", "Ibuprofen", "severe", "Competition at COX-1 binding site",
             "Ibuprofen blunts the cardioprotective antiplatelet effect of aspirin.",
             "Give aspirin at least 2 hours before ibuprofen, or use paracetamol."),
            ("Lisinopril", "Diclofenac", "severe", "Reduced renal prostaglandin synthesis",
             "Concurrent NSAID and ACE inhibitor may precipitate acute kidney injury.",
             "Avoid in renal impairment; monitor creatinine and potassium."),
            ("Losartan", "Hydrochlorothiazide", "mild", "Additive antihypertensive effect",
             "Enhanced blood pressure lowering, occasionally symptomatic hypotension.",
             "Usually intended; monitor postural blood pressure."),
            ("Furosemide", "Lisinopril", "moderate", "Volume depletion plus RAAS blockade",
             "First-dose hypotension and rising creatinine.",
             "Start low, review renal function within a week."),
            ("Carbamazepine", "Fluoxetine", "moderate", "CYP3A4 inhibition",
             "Raised carbamazepine levels causing ataxia and drowsiness.",
             "Monitor levels; consider an alternative antidepressant."),
            ("Metronidazole", "Sodium Valproate", "mild", "Uncertain",
             "Reports of raised valproate levels.", "Clinical monitoring is sufficient."),
            ("Ciprofloxacin", "Ferrous Sulphate", "moderate", "Chelation in the gut",
             "Iron markedly reduces ciprofloxacin absorption.",
             "Separate doses by at least 2 hours."),
            ("Omeprazole", "Ferrous Sulphate", "mild", "Reduced gastric acidity",
             "Impaired iron absorption.", "Take iron with vitamin C, review response."),
            ("Amitriptyline", "Sertraline", "severe", "Serotonergic and anticholinergic additive effect",
             "Risk of serotonin syndrome and QT prolongation.",
             "Avoid combination; if unavoidable, ECG and close monitoring."),
            ("Prednisolone", "Diclofenac", "moderate", "Additive gastric mucosal injury",
             "Increased risk of peptic ulceration and bleeding.",
             "Add gastroprotection; use the shortest possible course."),
            ("Atorvastatin", "Ciprofloxacin", "mild", "CYP3A4 competition",
             "Small increase in statin exposure and myalgia risk.", "Advise reporting muscle pain."),
            ("Morphine", "Amitriptyline", "moderate", "Additive CNS depression",
             "Excessive sedation and respiratory depression.", "Reduce doses; monitor sedation score."),
        ]
        rows = []
        for i, (d1, d2, sev, mech, effect, mgmt) in enumerate(pairs, start=1):
            rows.append((i, self.med_id[d1], self.med_id[d2], sev, mech, effect, mgmt))
        w.insert("drug_interactions",
                 ["id", "drug1_id", "drug2_id", "severity", "mechanism", "clinical_effect", "management"], rows)

        w.comment("Allergen catalogue")
        self.allergen_id = {}
        rows = []
        for i, (name, cls, cross) in enumerate(ALLERGENS, start=1):
            self.allergen_id[name] = i
            rows.append((i, name, cls, cross))
        w.insert("allergen_catalog", ["id", "name", "allergen_class", "cross_reactivity"], rows)

        w.comment("Laboratory test catalogue")
        self.lab_tests_by_panel: dict[str, list[tuple]] = {}
        rows = []
        for i, t in enumerate(LAB_TESTS, start=1):
            panel, test, spec, unit, low, high, ref_text, tat, price = t
            self.lab_tests_by_panel.setdefault(panel, []).append(t)
            rows.append((i, panel, test, spec, unit, low, high, ref_text, tat, price))
        w.insert("lab_test_catalog",
                 ["id", "panel_name", "test_name", "specimen_type", "result_unit",
                  "ref_low", "ref_high", "ref_text", "turnaround_hours", "price_kes"], rows)
        self.panel_price = {p: sum(t[8] for t in ts) for p, ts in self.lab_tests_by_panel.items()}

        w.comment("Vaccine catalogue")
        rows = [(i, n, d, doses, route, routine) for i, (n, d, doses, route, routine) in enumerate(VACCINES, start=1)]
        w.insert("vaccine_catalog", ["id", "name", "target_disease", "doses_required", "route", "is_routine"], rows)

        w.comment("Insurance providers")
        rows = []
        for i, (name, short, kind) in enumerate(INSURERS, start=1):
            rows.append((i, name, short, kind, f"020{rng.randint(1000000, 9999999)}",
                         f"claims@{short.lower()}.co.ke", True))
        w.insert("insurance_providers", ["id", "name", "short_name", "type", "contact_phone", "claim_email", "is_active"], rows)

    # -- staff ---------------------------------------------------------------

    def gen_providers(self) -> None:
        w, rng = self.w, self.rng
        self.providers: list[dict] = []
        pid = 0

        def add(role, specialty, dept_code, title, fee=None):
            nonlocal pid
            pid += 1
            gender = "male" if self.chance(0.45) else "female"
            first = self.pick(FIRST_M if gender == "male" else FIRST_F)
            last = self.pick(SURNAMES)
            rec = {
                "id": pid, "role": role, "specialty": specialty, "dept": dept_code,
                "first": first, "last": last, "gender": gender, "title": title,
                "fee": fee,
                "hire": HISTORY_START - dt.timedelta(days=rng.randint(200, 4500)),
                "employment": rng.choices(["permanent", "contract", "locum", "intern", "visiting"],
                                          [60, 20, 8, 8, 4])[0],
            }
            self.providers.append(rec)
            return rec

        for specialty, dept, count in DOCTOR_SPECIALTIES:
            for i in range(count):
                title = "Consultant" if i == 0 else self.pick(["Consultant", "Senior Medical Officer", "Medical Officer", "Registrar"])
                fee = {"Consultant": 3000, "Senior Medical Officer": 2000,
                       "Medical Officer": 1500, "Registrar": 1500}[title]
                add("doctor", specialty, dept, title, fee)

        nurse_depts = ["EMERG", "GENMED", "GENMED", "SURG", "PAEDS", "OBGYN", "ICU", "ICU",
                       "ORTHO", "RENAL", "PSYCH", "ONCOL", "EMERG", "GENMED", "SURG", "OBGYN"]
        for dept in nurse_depts:
            add("nurse", None, dept, self.pick(["Registered Nurse", "Senior Nurse", "Nurse Officer", "Ward Manager"]))
        for _ in range(5):
            add("pharmacist", None, "PHARM", self.pick(["Pharmacist", "Pharmaceutical Technologist"]))
        for _ in range(5):
            add("lab_tech", None, "LAB", self.pick(["Laboratory Technologist", "Senior Lab Technologist"]))
        for _ in range(2):
            add("radiologist", "Radiology", "RAD", "Consultant Radiologist", 2500)
        for _ in range(2):
            add("physiotherapist", None, "ORTHO", "Physiotherapist")
        add("nutritionist", None, "GENMED", "Clinical Nutritionist")
        add("counsellor", None, "PSYCH", "Clinical Counsellor")
        for _ in range(4):
            add("admin", None, "RECORDS", self.pick(["Records Officer", "Front Desk Clerk", "Billing Officer"]))

        used_emails = set()
        rows = []
        for p in self.providers:
            base = f"{p['first'].lower()}.{p['last'].lower()}"
            email = f"{base}@hospital.co.ke"
            n = 2
            while email in used_emails:
                email = f"{base}{n}@hospital.co.ke"
                n += 1
            used_emails.add(email)
            p["email"] = email
            dept_name = next(d[1] for d in DEPARTMENTS if d[0] == p["dept"])
            rows.append((p["id"], f"EMP{p['id']:04d}", p["first"], p["last"], p["specialty"],
                         p["role"], dept_name, self.dept_id[p["dept"]], p["title"],
                         f"07{rng.randint(10000000, 99999999)}", p["email"], p["gender"],
                         p["hire"], p["employment"], p["fee"], True))
        w.comment("Providers — clinical and support staff")
        w.insert("providers",
                 ["id", "employee_no", "first_name", "last_name", "specialty", "role", "department",
                  "department_id", "job_title", "phone", "email", "gender", "hire_date",
                  "employment_type", "consultation_fee_kes", "is_active"], rows)

        # Department heads: the most senior doctor in each clinical department.
        head_rows = []
        for code, name, dtype, _, _ in DEPARTMENTS:
            heads = [p for p in self.providers if p["dept"] == code and p["title"].startswith("Consultant")]
            if heads:
                head_rows.append(f"UPDATE departments SET head_provider_id = {heads[0]['id']} WHERE code = {q(code)};")
        w.comment("Department heads")
        w.parts.append("\n".join(head_rows) + "\n")

        w.comment("Practising licences")
        regulators = {"doctor": ("Kenya Medical Practitioners and Dentists Council", "Medical Practitioner"),
                      "nurse": ("Nursing Council of Kenya", "Registered Nurse"),
                      "pharmacist": ("Pharmacy and Poisons Board", "Pharmacist"),
                      "lab_tech": ("Kenya Medical Laboratory Technicians Board", "Laboratory Technologist"),
                      "radiologist": ("Kenya Medical Practitioners and Dentists Council", "Radiologist"),
                      "physiotherapist": ("Physiotherapy Council of Kenya", "Physiotherapist"),
                      "nutritionist": ("Kenya Nutritionists and Dieticians Institute", "Nutritionist"),
                      "counsellor": ("Counsellors and Psychologists Board", "Counsellor")}
        rows, lic_id = [], 0
        for p in self.providers:
            if p["role"] not in regulators:
                continue
            lic_id += 1
            regulator, ltype = regulators[p["role"]]
            issued = p["hire"] + dt.timedelta(days=rng.randint(0, 300))
            expires = issued + dt.timedelta(days=365 * rng.randint(1, 5))
            while expires < TODAY - dt.timedelta(days=400):
                expires += dt.timedelta(days=365)
            rows.append((lic_id, p["id"], regulator, f"{p['role'][:3].upper()}-{20000 + p['id'] * 7}",
                         ltype, issued, expires, expires >= TODAY))
        w.insert("provider_licenses",
                 ["id", "provider_id", "regulator", "license_no", "license_type",
                  "issued_on", "expires_on", "is_current"], rows)

        w.comment("Weekly clinic schedules")
        self.schedules: list[dict] = []
        rows, sid = [], 0
        clinicians = [p for p in self.providers if p["role"] in ("doctor", "radiologist")]
        for p in clinicians:
            for weekday in rng.sample(range(1, 6), rng.randint(2, 4)):
                sid += 1
                start_h = self.pick([8, 9, 14])
                end_h = start_h + self.pick([3, 4])
                slot = self.pick([15, 20, 30])
                sched = {
                    "id": sid, "provider": p, "weekday": weekday,
                    "start": dt.time(start_h, 0), "end": dt.time(end_h, 0), "slot": slot,
                    "dept": p["dept"],
                }
                self.schedules.append(sched)
                dept_name = next(d[1] for d in DEPARTMENTS if d[0] == p["dept"])
                rows.append((sid, p["id"], self.dept_id[p["dept"]], weekday, sched["start"].strftime("%H:%M"),
                             sched["end"].strftime("%H:%M"), slot, f"Room {rng.randint(1, 24)}",
                             f"{dept_name} Clinic", (end_h - start_h) * 60 // slot,
                             HISTORY_START, None, True))
        w.insert("provider_schedules",
                 ["id", "provider_id", "department_id", "weekday", "start_time", "end_time",
                  "slot_minutes", "room", "clinic_name", "max_patients", "valid_from", "valid_to", "is_active"], rows)
        self.sched_by_provider: dict[int, list[dict]] = {}
        for s in self.schedules:
            self.sched_by_provider.setdefault(s["provider"]["id"], []).append(s)

        w.comment("Staff leave and absence")
        rows, off_id = [], 0
        for p in self.providers:
            for _ in range(rng.randint(0, 3)):
                off_id += 1
                start = HISTORY_START + dt.timedelta(days=rng.randint(0, (FUTURE_END - HISTORY_START).days - 20))
                reason = rng.choices(["annual_leave", "sick_leave", "study_leave", "conference",
                                      "maternity", "compassionate", "off_duty"],
                                     [40, 20, 10, 10, 6, 6, 8])[0]
                length = {"annual_leave": rng.randint(5, 21), "sick_leave": rng.randint(1, 7),
                          "study_leave": rng.randint(5, 30), "conference": rng.randint(2, 5),
                          "maternity": 90, "compassionate": rng.randint(1, 5),
                          "off_duty": rng.randint(1, 3)}[reason]
                rows.append((off_id, p["id"], start, start + dt.timedelta(days=length), reason,
                             self.chance(0.92), None))
        w.insert("provider_time_off",
                 ["id", "provider_id", "start_date", "end_date", "reason", "is_approved", "notes"], rows)

        self.doctors = [p for p in self.providers if p["role"] == "doctor"]
        self.nurses = [p for p in self.providers if p["role"] == "nurse"]
        self.pharmacists = [p for p in self.providers if p["role"] == "pharmacist"]
        self.lab_techs = [p for p in self.providers if p["role"] == "lab_tech"]
        self.radiologists = [p for p in self.providers if p["role"] == "radiologist"]
        self.clerks = [p for p in self.providers if p["role"] == "admin"]
        self.anaesthetists = [p for p in self.doctors if p["specialty"] == "Anaesthesiology"]
        self.doctors_by_dept: dict[str, list[dict]] = {}
        for d in self.doctors:
            self.doctors_by_dept.setdefault(d["dept"], []).append(d)
        self.nurses_by_dept: dict[str, list[dict]] = {}
        for n in self.nurses:
            self.nurses_by_dept.setdefault(n["dept"], []).append(n)

    def doctor_for(self, dept_code: str) -> dict:
        pool = self.doctors_by_dept.get(dept_code) or self.doctors_by_dept.get("GENMED") or self.doctors
        return self.pick(pool)

    def nurse_for(self, dept_code: str) -> dict:
        pool = self.nurses_by_dept.get(dept_code) or self.nurses
        return self.pick(pool)

    # -- patients ------------------------------------------------------------

    def gen_patients(self) -> None:
        w, rng = self.w, self.rng
        self.patients: list[dict] = []
        used_names: set[tuple] = set()

        for i in range(1, self.n_patients + 1):
            gender = "female" if self.chance(0.56) else "male"
            while True:
                first = self.pick(FIRST_F if gender == "female" else FIRST_M)
                middle = self.pick(SURNAMES)
                last = self.pick(SURNAMES)
                if (first, middle, last) not in used_names:
                    used_names.add((first, middle, last))
                    break
            # Age profile: a hospital sees more children and older adults.
            band = rng.choices(["child", "young", "adult", "older", "elderly"], [16, 22, 30, 20, 12])[0]
            age = {"child": rng.randint(0, 14), "young": rng.randint(15, 29),
                   "adult": rng.randint(30, 49), "older": rng.randint(50, 69),
                   "elderly": rng.randint(70, 92)}[band]
            dob = TODAY - dt.timedelta(days=age * 365 + rng.randint(0, 364))
            county = rng.randint(1, len(COUNTIES))
            reg_day = HISTORY_START + dt.timedelta(days=rng.randint(0, (TODAY - HISTORY_START).days - 10))
            registered = at(reg_day, rng.randint(7, 18), rng.randint(0, 59))
            deceased = self.chance(0.035) and age > 40
            patient = {
                "id": self.uid(), "no": f"PT-{i:05d}", "first": first, "middle": middle,
                "last": last, "dob": dob, "age": age, "gender": gender, "county": county,
                "registered": registered,
                "deceased": deceased,
                "blood": rng.choices(BLOOD, BLOOD_W)[0],
            }
            self.patients.append(patient)

        rows = []
        for p in self.patients:
            age = p["age"]
            marital = "single" if age < 20 else rng.choices(MARITAL, [25, 50, 8, 10, 7])[0]
            occupation = "Student" if age < 18 else ("Retired" if age >= 65 and self.chance(0.6) else self.pick(OCCUPATIONS))
            education = "None" if age < 6 else ("Primary" if age < 14 else self.pick(EDUCATION))
            county_name = COUNTIES[p["county"] - 1][0]
            dod = None
            if p["deceased"]:
                dod = TODAY - dt.timedelta(days=self.rng.randint(30, 700))
            rows.append((
                p["id"], p["no"], f"{20000000 + self.rng.randint(0, 19999999)}" if age >= 18 else None,
                p["first"], p["middle"], p["last"], p["dob"], p["gender"], p["blood"],
                f"07{self.rng.randint(10000000, 99999999)}",
                f"{p['first'].lower()}.{p['last'].lower()}{self.rng.randint(1, 99)}@mail.co.ke",
                f"P.O. Box {self.rng.randint(100, 9999)}, {county_name}",
                p["county"], f"{county_name} {self.pick(SUB_COUNTIES)}", marital, occupation, education,
                f"{self.pick(FIRST_M + FIRST_F)} {self.pick(SURNAMES)}",
                f"07{self.rng.randint(10000000, 99999999)}",
                self.pick(["Spouse", "Parent", "Sibling", "Child", "Guardian", "Friend"]),
                self.pick(LANGUAGES), p["deceased"], dod, not p["deceased"], p["registered"]))
        w.comment(f"Patients ({len(rows)})")
        w.insert("patients",
                 ["id", "patient_no", "national_id", "first_name", "middle_name", "last_name",
                  "date_of_birth", "gender", "blood_type", "phone_primary", "email", "address",
                  "county_id", "sub_county", "marital_status", "occupation", "education_level",
                  "next_of_kin", "nok_phone", "nok_relation", "preferred_language",
                  "is_deceased", "date_of_death", "is_active", "registered_at"], rows)

    def gen_patient_background(self) -> None:
        w, rng = self.w, self.rng

        w.comment("Known allergies")
        rows, aid = [], 0
        for p in self.patients:
            for name in rng.sample([a[0] for a in ALLERGENS], rng.choices([0, 1, 2, 3], [45, 33, 16, 6])[0]):
                aid += 1
                severity = rng.choices(["mild", "moderate", "severe", "life_threatening"], [34, 36, 22, 8])[0]
                onset = p["dob"] + dt.timedelta(days=rng.randint(365, max(400, p["age"] * 365)))
                rows.append((aid, p["id"], self.allergen_id[name], self.pick(REACTIONS), severity,
                             min(onset, TODAY), self.pick(self.doctors)["id"], True,
                             "Documented at registration." if self.chance(0.4) else None))
        w.insert("patient_allergies",
                 ["id", "patient_id", "allergen_id", "reaction", "severity", "onset_date",
                  "recorded_by", "is_active", "notes"], rows)

        w.comment("Past medical history")
        rows, hid = [], 0
        chronic_pool = [(c[0], next(x[1] for x in ICD10 if x[0] == c[0])) for c in CONDITIONS if c[6]]
        for p in self.patients:
            count = rng.choices([0, 1, 2, 3], [30, 36, 24, 10])[0] if p["age"] > 15 else rng.choices([0, 1], [75, 25])[0]
            for code, desc in rng.sample(chronic_pool, min(count, len(chronic_pool))):
                hid += 1
                diagnosed = TODAY - dt.timedelta(days=rng.randint(200, max(400, p["age"] * 200)))
                resolved = None
                active = True
                if self.chance(0.18):
                    resolved = diagnosed + dt.timedelta(days=rng.randint(60, 900))
                    if resolved > TODAY:
                        resolved = None
                    else:
                        active = False
                rows.append((hid, p["id"], code, desc, diagnosed, resolved, True, active,
                             self.pick(["Managed in the outpatient clinic.",
                                        "Stable on current therapy.",
                                        "Poor adherence documented previously.",
                                        "Diagnosed at another facility.", None])))
        w.insert("patient_medical_history",
                 ["id", "patient_id", "icd10_code", "condition_name", "diagnosed_date",
                  "resolved_date", "is_chronic", "is_active", "notes"], rows)

        w.comment("Family history")
        fam_conditions = ["Hypertension", "Type 2 Diabetes", "Stroke", "Ischaemic Heart Disease",
                          "Breast Cancer", "Asthma", "Tuberculosis", "Sickle Cell Disease",
                          "Epilepsy", "Chronic Kidney Disease", "Colorectal Cancer", "Depression"]
        rows, fid = [], 0
        for p in self.patients:
            for _ in range(rng.choices([0, 1, 2, 3], [34, 32, 24, 10])[0]):
                fid += 1
                rows.append((fid, p["id"],
                             rng.choices(["mother", "father", "sibling", "grandmother", "grandfather", "aunt", "uncle"],
                                         [22, 18, 16, 14, 12, 9, 9])[0],
                             self.pick(fam_conditions), rng.randint(28, 78), self.chance(0.35)))
        w.insert("patient_family_history",
                 ["id", "patient_id", "relation", "condition_name", "age_at_onset", "is_deceased"], rows)

        w.comment("Immunisations")
        rows, iid = [], 0
        for p in self.patients:
            if p["age"] <= 16:
                schedule = [(1, 0), (2, 45), (3, 75), (4, 105), (5, 270), (6, 545)]
                for vac_idx, offset in schedule:
                    if p["dob"] + dt.timedelta(days=offset) > TODAY:
                        break
                    iid += 1
                    nurse = self.pick(self.nurses)
                    rows.append((iid, p["id"], vac_idx, 1, p["dob"] + dt.timedelta(days=offset),
                                 nurse["id"], f"LOT{rng.randint(10000, 99999)}",
                                 self.pick(["Left thigh", "Right thigh", "Left deltoid", "Oral"]),
                                 "Mild fever for 24 hours" if self.chance(0.05) else None))
            for _ in range(rng.choices([0, 1, 2], [45, 38, 17])[0]):
                iid += 1
                vac_idx = rng.randint(7, len(VACCINES))
                given = TODAY - dt.timedelta(days=rng.randint(30, 1200))
                nurse = self.pick(self.nurses)
                rows.append((iid, p["id"], vac_idx, rng.randint(1, 2), given, nurse["id"],
                             f"LOT{rng.randint(10000, 99999)}",
                             self.pick(["Left deltoid", "Right deltoid"]), None))
        w.insert("immunizations",
                 ["id", "patient_id", "vaccine_id", "dose_number", "administered_on",
                  "administered_by", "batch_no", "site", "adverse_event"], rows)

        w.comment("Insurance cover")
        rows, insid = [], 0
        self.insurance_for: dict[str, int] = {}
        for p in self.patients:
            if not self.chance(0.62):
                continue
            insid += 1
            provider = rng.choices(range(1, len(INSURERS) + 1), [30, 12, 11, 9, 9, 7, 8, 6, 4, 4])[0]
            valid_from = p["registered"].date()
            valid_to = valid_from + dt.timedelta(days=365 * rng.randint(1, 4))
            active = valid_to >= TODAY
            rows.append((insid, p["id"], provider, f"POL{rng.randint(100000, 999999)}",
                         f"MEM{rng.randint(10000, 99999)}",
                         self.pick(["Inpatient & Outpatient", "Inpatient only", "Comprehensive", "Outpatient only"]),
                         self.pick([100000, 200000, 300000, 500000, 1000000]),
                         valid_from, valid_to, True, active))
            if active:
                self.insurance_for[p["id"]] = insid
        w.insert("patient_insurance",
                 ["id", "patient_id", "provider_id", "policy_number", "member_no", "coverage_type",
                  "coverage_limit_kes", "valid_from", "valid_to", "is_primary", "is_active"], rows)

    # -- the clinical journey ------------------------------------------------

    def gen_journeys(self) -> None:
        """Build appointments → encounters → everything downstream, per patient."""
        rng = self.rng
        self.appointments: list[tuple] = []
        self.encounters: list[tuple] = []
        self.triage: list[tuple] = []
        self.vitals: list[tuple] = []
        self.diagnoses: list[tuple] = []
        self.notes: list[tuple] = []
        self.admissions: list[tuple] = []
        self.bed_assignments: list[tuple] = []
        self.transfers: list[tuple] = []
        self.referrals: list[tuple] = []
        self.procedures: list[tuple] = []
        self.surgeries: list[tuple] = []
        self.prescriptions: list[tuple] = []
        self.rx_items: list[tuple] = []
        self.mar: list[tuple] = []
        self.lab_orders: list[tuple] = []
        self.lab_results: list[tuple] = []
        self.imaging: list[tuple] = []
        self.alerts: list[tuple] = []
        self.bills: list[tuple] = []
        self.bill_items: list[tuple] = []
        self.claims: list[tuple] = []
        self.payments: list[tuple] = []
        # Ancillary services, filled after the journeys are known.
        self.anc_visits: list[tuple] = []
        self.deliveries: list[tuple] = []
        self.newborns: list[tuple] = []
        self.mortality: list[tuple] = []
        self.stock_batches: list[tuple] = []
        self.stock_movements: list[tuple] = []
        self.blood_units: list[tuple] = []
        self.transfusions: list[tuple] = []
        self.shifts: list[tuple] = []
        self.documents: list[tuple] = []
        self.consents: list[tuple] = []
        self.incidents: list[tuple] = []
        self.enrollments: list[tuple] = []
        self.equipment: list[tuple] = []
        self.equipment_maint: list[tuple] = []
        self.notifiable: list[tuple] = []
        self.feedback: list[tuple] = []
        self.access_log: list[tuple] = []
        # Cross-references the ancillary pass needs.
        self.encounter_index: list[dict] = []
        self.maternity_events: list[dict] = []
        self.death_events: list[dict] = []
        self.surgery_events: list[dict] = []

        # Beds held by an admission that has not been discharged. A real ward can
        # never put two patients in one bed, and occupancy questions are only
        # meaningful if that holds.
        self.occupied_beds: set[int] = set()

        self.ids = {k: 0 for k in
                    ["appt", "enc", "triage", "vital", "dx", "note", "adm", "bed_assign", "transfer",
                     "referral", "proc", "surg", "rx", "rx_item", "mar", "lab", "lab_res",
                     "img", "alert", "bill", "bill_item", "claim", "pay",
                     "anc", "delivery", "newborn", "mortality", "batch", "movement",
                     "blood", "transfusion", "shift", "doc", "consent", "incident",
                     "enrollment", "equip", "equip_maint", "notifiable", "feedback", "access"]}

        for patient in self.patients:
            self.build_patient_journey(patient)

        # A hospital is never empty. Fill roughly 55% of beds with patients who are
        # still in, so occupancy, "who is admitted right now" and bed-availability
        # questions have real answers.
        self.gen_current_census()

        # Future bookings: clinics that have not happened yet.
        for _ in range(int(self.n_patients * 0.9)):
            patient = self.pick(self.patients)
            if patient["deceased"]:
                continue
            self.future_appointment(patient)

    def gen_current_census(self) -> None:
        rng = self.rng
        target = int(self.total_beds * 0.55)
        admittable = [c for c in CONDITIONS if c[7] >= 0.2]
        living = [p for p in self.patients if not p["deceased"]]
        attempts = 0
        while len(self.occupied_beds) < target and attempts < target * 6:
            attempts += 1
            patient = self.pick(living)
            profile = self.pick(admittable)
            if profile[8] == "maternity" and patient["gender"] == "male":
                continue
            if patient["age"] < 15 and profile[1] not in ("PAEDS", "GENMED", "ENT"):
                continue
            day = TODAY - dt.timedelta(days=rng.randint(0, 24))
            self.build_encounter(patient, profile, day, is_first=False,
                                 force_admit=True, force_open=True)

    def free_bed(self, ward_id: int) -> tuple[int, int]:
        """An unoccupied bed in this ward, falling back to any ward with space."""
        free = [b for b in self.beds_by_ward[ward_id] if b not in self.occupied_beds]
        if free:
            return self.pick(free), ward_id
        # Overflow goes to the emptiest ward rather than the first one with a gap,
        # so a full medical ward does not leave every other ward untouched.
        by_space = sorted(
            self.beds_by_ward.items(),
            key=lambda kv: -len([b for b in kv[1] if b not in self.occupied_beds]))
        for other, bed_ids in by_space:
            free = [b for b in bed_ids if b not in self.occupied_beds]
            if free:
                return self.pick(free), other
        return self.pick(self.beds_by_ward[ward_id]), ward_id

    # -- ancillary hospital services -----------------------------------------

    def gen_ancillary(self) -> None:
        """Everything that hangs off the clinical journeys rather than driving them."""
        self.gen_maternity()
        self.gen_mortality()
        self.gen_pharmacy_stock()
        self.gen_blood_bank()
        self.gen_rota()
        self.gen_documents_and_consent()
        self.gen_programs()
        self.gen_equipment()
        self.gen_public_health()
        self.gen_feedback()
        self.gen_incidents()
        self.gen_access_log()

    def gen_maternity(self) -> None:
        rng = self.rng
        obgyn_doctors = self.doctors_by_dept.get("OBGYN", self.doctors)
        midwives = self.nurses_by_dept.get("OBGYN", self.nurses)

        for event in self.maternity_events:
            patient = event["patient"]
            caesarean = event["icd"] == "O82"
            delivered_at = event["when"] + dt.timedelta(hours=rng.randint(1, 14))
            gestation = rng.choices([32, 35, 37, 38, 39, 40, 41, 42], [3, 5, 12, 22, 25, 22, 8, 3])[0]
            mode = ("caesarean" if caesarean else
                    rng.choices(["spontaneous_vaginal", "vacuum_assisted", "forceps", "breech"],
                                [88, 6, 3, 3])[0])
            multiple = self.chance(0.03)
            stillbirth = self.chance(0.02)
            blood_loss = rng.randint(600, 1800) if caesarean else rng.randint(150, 700)
            if self.chance(0.04):
                blood_loss = rng.randint(1000, 2500)   # postpartum haemorrhage
            delivery_id = self.next_id("delivery")
            self.deliveries.append((
                delivery_id, patient["id"], event["enc_id"], event["admission_id"], delivered_at,
                mode, "no_labour" if caesarean and self.chance(0.4) else
                rng.choices(["spontaneous", "induced"], [78, 22])[0],
                gestation, round(rng.uniform(1.5, 26.0), 1),
                self.pick(obgyn_doctors)["id"],
                "spinal" if caesarean else ("none" if self.chance(0.8) else "local"),
                (not caesarean) and self.chance(0.22),
                "none" if caesarean else rng.choices(
                    ["none", "first_degree", "second_degree", "third_degree"], [58, 25, 14, 3])[0],
                blood_loss, self.chance(0.96),
                self.pick(["Postpartum haemorrhage managed with uterotonics.",
                           "Prolonged second stage.", "Meconium-stained liquor.",
                           "Shoulder dystocia resolved with McRoberts manoeuvre."])
                if self.chance(0.18) else None,
                "multiple_birth" if multiple else ("stillbirth" if stillbirth else "live_birth")))

            for order in range(1, (2 if multiple else 1) + 1):
                weight = int(rng.gauss(3150, 520))
                if gestation < 37:
                    weight = int(rng.gauss(2150, 450))
                weight = max(700, min(weight, 4800))
                dead = stillbirth or (weight < 1500 and self.chance(0.25))
                apgar1 = rng.choices([9, 8, 7, 5, 3, 0], [40, 28, 16, 9, 5, 2])[0] if not stillbirth else 0
                apgar5 = min(10, apgar1 + rng.randint(0, 2)) if not stillbirth else 0
                self.newborns.append((
                    self.next_id("newborn"), delivery_id, patient["id"], None, order,
                    "female" if self.chance(0.49) else "male", weight,
                    round(rng.uniform(44, 54), 1), round(rng.uniform(31, 37), 1),
                    apgar1, apgar5, apgar1 < 7,
                    weight < 2500 or apgar5 < 7,
                    "stillborn" if stillbirth else ("neonatal_death" if dead else "live"),
                    f"BN{self.ids['newborn']:07d}",
                    "Admitted to the newborn unit for prematurity." if weight < 2500 and not dead else None))

        # Antenatal profiles: mothers who delivered here, plus current pregnancies.
        mothers = {e["patient"]["id"]: e for e in self.maternity_events}
        candidates = [p for p in self.patients
                      if p["gender"] == "female" and 15 <= p["age"] <= 45 and not p["deceased"]]
        pregnant_now = [p for p in candidates if p["id"] not in mothers and self.chance(0.12)]

        for event in self.maternity_events:
            patient = event["patient"]
            edd = event["when"].date()
            visits = rng.randint(2, 8)
            for n in range(1, visits + 1):
                visit_day = edd - dt.timedelta(days=int((visits - n + 1) * rng.uniform(18, 34)))
                self.emit_anc(patient, None, n, visit_day,
                              max(6, 40 - int((edd - visit_day).days / 7)), midwives)
        for patient in pregnant_now:
            weeks = rng.randint(8, 38)
            visits = max(1, weeks // 8)
            for n in range(1, visits + 1):
                gest = min(40, 8 + (n - 1) * 6)
                visit_day = TODAY - dt.timedelta(days=int((weeks - gest) * 7))
                if visit_day > TODAY:
                    continue
                self.emit_anc(patient, None, n, visit_day, gest, midwives)

    def emit_anc(self, patient, enc_id, number, visit_day, gestation, midwives) -> None:
        rng = self.rng
        if visit_day < HISTORY_START or visit_day > TODAY:
            return
        systolic = rng.randint(102, 132)
        protein = "Negative"
        if self.chance(0.08):
            systolic = rng.randint(142, 168)
            protein = self.pick(["Trace", "1+", "2+"])
        self.anc_visits.append((
            self.next_id("anc"), patient["id"], enc_id, number, visit_day, gestation,
            gestation + rng.randint(-3, 3) if gestation > 20 else None,
            rng.randint(120, 160) if gestation > 20 else None,
            self.pick(["Cephalic", "Breech", "Transverse", "Not palpable"]) if gestation > 28 else None,
            systolic, systolic - rng.randint(35, 50), round(rng.uniform(52, 92), 1),
            protein, round(rng.uniform(8.5, 13.8), 1),
            self.pick(["Previous caesarean section", "Grand multipara", "Anaemia in pregnancy",
                       "Raised blood pressure", "Advanced maternal age"]) if self.chance(0.22) else None,
            self.pick(midwives)["id"],
            visit_day + dt.timedelta(days=rng.choice([14, 21, 28])), None))

    def gen_mortality(self) -> None:
        rng = self.rng
        seen = set()
        for event in self.death_events:
            patient = event["patient"]
            if patient["id"] in seen:
                continue
            seen.add(patient["id"])
            died_at = event["when"]
            place = ("icu" if event["severity"] == "severe" and self.chance(0.5) else
                     "theatre" if event["severity"] == "surgical" and self.chance(0.2) else
                     "maternity" if event["dept_code"] == "OBGYN" else
                     "casualty" if self.chance(0.2) else "ward")
            underlying = self.pick(ICD10)[0]
            self.mortality.append((
                self.next_id("mortality"), patient["id"], event["enc_id"], event["admission_id"],
                died_at, place, event["icd"], underlying,
                self.pick(["Hypertension", "Type 2 diabetes mellitus", "HIV disease",
                           "Chronic kidney disease", None]),
                event["doctor"]["id"], f"DC{self.ids['mortality']:06d}",
                self.chance(0.18), self.chance(0.1),
                died_at + dt.timedelta(hours=rng.randint(1, 6)),
                died_at + dt.timedelta(days=rng.randint(1, 14)) if self.chance(0.8) else None,
                self.chance(0.75),
                self.pick(["Family counselled by the ward team.", "Body released to the family.",
                           "Referred to the coroner.", None])))

    def gen_pharmacy_stock(self) -> None:
        rng = self.rng
        pharmacists = self.pharmacists
        balances: dict[int, int] = {}
        for med_id in range(1, len(MEDICATIONS) + 1):
            med = MEDICATIONS[med_id - 1]
            for b in range(rng.randint(1, 3)):
                received = TODAY - dt.timedelta(days=rng.randint(20, 700))
                shelf_life = rng.randint(180, 1080)
                qty = rng.choice([200, 500, 1000, 2000, 5000])
                # A realistic store has some expired and some nearly-empty lines.
                on_hand = int(qty * rng.uniform(0, 0.85))
                if self.chance(0.08):
                    on_hand = 0
                batch_id = self.next_id("batch")
                self.stock_batches.append((
                    batch_id, med_id, f"B{med_id:03d}-{received.year}{b + 1}",
                    received + dt.timedelta(days=shelf_life), qty, on_hand,
                    round(float(med[8]) * rng.uniform(0.45, 0.75), 2),
                    self.pick(["Kenya Medical Supplies Authority", "Mission for Essential Drugs",
                               "Surgipharm Ltd", "Laborex Kenya", "Harleys Ltd"]),
                    received, self.pick(pharmacists)["id"],
                    rng.choices(["main", "ward", "theatre", "emergency"], [70, 16, 8, 6])[0]))
                balances[med_id] = balances.get(med_id, 0) + on_hand
                self.stock_movements.append((
                    self.next_id("movement"), med_id, batch_id, "receipt", qty,
                    at(received, rng.randint(8, 16), 0), self.pick(pharmacists)["id"],
                    None, "Delivery against purchase order", balances[med_id]))

        # Dispensing draws stock down against the prescription that consumed it.
        batch_by_med: dict[int, list[int]] = {}
        for row in self.stock_batches:
            batch_by_med.setdefault(row[1], []).append(row[0])
        rx_by_id = {row[0]: row for row in self.prescriptions}
        for item in self.rx_items:
            item_id, rx_id, med_id, *_rest = item
            if not item[9]:                     # is_dispensed
                continue
            qty = item[6]
            dispensed_at = item[10]
            balances[med_id] = max(0, balances.get(med_id, 0) - qty)
            self.stock_movements.append((
                self.next_id("movement"), med_id, self.pick(batch_by_med.get(med_id, [None])),
                "dispense", -qty, dispensed_at, self.pick(self.pharmacists)["id"], rx_id,
                "Outpatient dispensing", balances[med_id]))

        for _ in range(int(self.n_patients * 0.4)):
            med_id = rng.randint(1, len(MEDICATIONS))
            kind = rng.choices(["issue_to_ward", "expiry", "adjustment", "return", "disposal"],
                               [44, 18, 18, 12, 8])[0]
            qty = rng.randint(5, 120)
            signed = -qty if kind in ("issue_to_ward", "expiry", "disposal") else qty
            balances[med_id] = max(0, balances.get(med_id, 0) + signed)
            when = at(TODAY - dt.timedelta(days=rng.randint(1, 600)), rng.randint(8, 17), 0)
            self.stock_movements.append((
                self.next_id("movement"), med_id, self.pick(batch_by_med.get(med_id, [None])),
                kind, signed, when, self.pick(self.pharmacists)["id"], None,
                {"issue_to_ward": "Ward top-up request", "expiry": "Expired stock written off",
                 "adjustment": "Stock count correction", "return": "Returned unused from the ward",
                 "disposal": "Damaged in storage"}[kind], balances[med_id]))

    def gen_blood_bank(self) -> None:
        rng = self.rng
        groups = ["O+", "O-", "A+", "A-", "B+", "B-", "AB+", "AB-"]
        weights = [38, 5, 24, 3, 18, 2, 8, 2]
        unit_ids: list[int] = []
        for _ in range(int(self.n_patients * 0.9)):
            collected = TODAY - dt.timedelta(days=rng.randint(0, 60))
            component = rng.choices(["whole_blood", "packed_cells", "platelets",
                                     "fresh_frozen_plasma", "cryoprecipitate"],
                                    [30, 44, 10, 12, 4])[0]
            shelf = {"whole_blood": 35, "packed_cells": 42, "platelets": 5,
                     "fresh_frozen_plasma": 365, "cryoprecipitate": 365}[component]
            expires = collected + dt.timedelta(days=shelf)
            screening = rng.choices(["passed", "pending", "failed"], [92, 5, 3])[0]
            if screening == "failed":
                status = "discarded"
            elif expires < TODAY:
                status = rng.choices(["transfused", "expired"], [70, 30])[0]
            else:
                status = rng.choices(["available", "reserved", "issued"], [76, 14, 10])[0]
            uid = self.next_id("blood")
            unit_ids.append(uid)
            self.blood_units.append((
                uid, f"BU{uid:06d}", rng.choices(groups, weights)[0], component,
                {"whole_blood": 450, "packed_cells": 280, "platelets": 60,
                 "fresh_frozen_plasma": 220, "cryoprecipitate": 40}[component],
                rng.choices(["voluntary_donation", "replacement_donation", "regional_blood_centre"],
                            [52, 30, 18])[0],
                collected, expires, screening, status,
                self.pick(["Blood bank fridge 1", "Blood bank fridge 2", "Platelet agitator",
                           "Plasma freezer"])))

        # Transfuse the units marked as transfused, into plausible recipients.
        candidates = [e for e in self.encounter_index
                      if e["icd"] in ("D50.9", "D57.1", "O82", "A41.9", "C34.9", "C50.9", "N18.5")
                      or e["severity"] in ("surgical", "severe")]
        transfused = [u for u in self.blood_units if u[9] == "transfused"]
        for unit in transfused:
            if not candidates:
                break
            event = self.pick(candidates)
            issued = event["when"] + dt.timedelta(hours=rng.randint(2, 40))
            started = issued + dt.timedelta(minutes=rng.randint(20, 120))
            reaction = rng.choices(["none", "febrile", "allergic", "circulatory_overload", "haemolytic"],
                                   [90, 5, 3, 1.5, 0.5])[0]
            self.transfusions.append((
                self.next_id("transfusion"), event["patient"]["id"], event["enc_id"],
                event["admission_id"], unit[0], event["doctor"]["id"],
                rng.choices(["compatible", "emergency_release", "incompatible"], [93, 6, 1])[0],
                self.pick(["Symptomatic anaemia", "Intra-operative blood loss",
                           "Postpartum haemorrhage", "Sickle cell crisis",
                           "Chronic kidney disease with anaemia", "Sepsis with coagulopathy"]),
                issued, started, started + dt.timedelta(hours=rng.randint(2, 4)),
                unit[4] - rng.randint(0, 40), self.pick(self.nurses)["id"], reaction,
                self.pick(["Transfusion stopped, hydrocortisone given, patient stable.",
                           "Paracetamol given, transfusion completed slowly."])
                if reaction != "none" else None))

    def gen_rota(self) -> None:
        rng = self.rng
        ward_ids = list(self.ward_id.values())
        for offset in range(-60, 8):
            day = TODAY + dt.timedelta(days=offset)
            for ward_id in ward_ids:
                for shift_type, start_h, hours in (("day", 8, 12), ("night", 20, 12)):
                    on = rng.sample(self.nurses, min(len(self.nurses), rng.randint(2, 3)))
                    for index, nurse in enumerate(on):
                        self.shifts.append((
                            self.next_id("shift"), nurse["id"], ward_id,
                            self.dept_id[nurse["dept"]], day, shift_type,
                            at(day, start_h, 0),
                            at(day, start_h, 0) + dt.timedelta(hours=hours),
                            self.pick(["Ward nurse", "Medication nurse", "Triage nurse", "Nurse in charge"]),
                            index == 0))
            for doctor in rng.sample(self.doctors, min(len(self.doctors), 4)):
                self.shifts.append((
                    self.next_id("shift"), doctor["id"], None, self.dept_id[doctor["dept"]],
                    day, "on_call", at(day, 17, 0), at(day, 17, 0) + dt.timedelta(hours=16),
                    f"On-call {doctor['specialty']}", False))

    def gen_documents_and_consent(self) -> None:
        rng = self.rng
        for event in self.encounter_index:
            patient = event["patient"]
            if self.chance(0.45):
                doc_type = rng.choices(
                    ["lab_report", "imaging_report", "referral_letter", "insurance",
                     "identification", "discharge_summary", "consent", "other"],
                    [22, 16, 14, 14, 10, 12, 8, 4])[0]
                uploaded = event["when"] + dt.timedelta(hours=rng.randint(1, 72))
                self.documents.append((
                    self.next_id("doc"), patient["id"], event["enc_id"], doc_type,
                    f"{doc_type.replace('_', ' ').title()} - {patient['no']}",
                    f"{patient['no']}_{doc_type}_{self.ids['doc']}.pdf", "application/pdf",
                    rng.randint(40_000, 3_500_000), self.pick(self.clerks)["id"], uploaded,
                    doc_type in ("consent", "discharge_summary"), None))

        for surgery in self.surgeries:
            enc_id, patient_id = surgery[2], surgery[3]
            signed = surgery[11] - dt.timedelta(hours=rng.randint(1, 20))
            for consent_type in ("surgery", "anaesthesia"):
                self.consents.append((
                    self.next_id("consent"), patient_id, enc_id, surgery[1], consent_type, True,
                    rng.choices(["patient", "guardian", "next_of_kin"], [82, 10, 8])[0],
                    self.pick(self.nurses)["id"], signed, None, None, None))
        for tx in self.transfusions:
            self.consents.append((
                self.next_id("consent"), tx[1], tx[2], None, "transfusion", True,
                rng.choices(["patient", "next_of_kin", "guardian"], [80, 14, 6])[0],
                self.pick(self.nurses)["id"], tx[8] or tx[9], None, None, None))
        for event in self.encounter_index:
            if self.chance(0.06):
                granted = self.chance(0.9)
                self.consents.append((
                    self.next_id("consent"), event["patient"]["id"], event["enc_id"], None,
                    self.pick(["hiv_test", "data_sharing", "photography", "research"]), granted,
                    "patient", self.pick(self.nurses)["id"],
                    event["when"] + dt.timedelta(minutes=rng.randint(10, 200)), None,
                    event["when"] + dt.timedelta(days=rng.randint(30, 400)) if not granted else None,
                    "Patient declined after counselling." if not granted else None))

    def gen_programs(self) -> None:
        rng = self.rng
        programs = [
            ("CCC", "Comprehensive Care Centre", "HIV care and antiretroviral therapy", "HIV disease", 90),
            ("TB", "TB Clinic", "Directly observed TB treatment", "Tuberculosis", 30),
            ("DM", "Diabetes Clinic", "Structured diabetes review", "Diabetes mellitus", 90),
            ("HTN", "Hypertension Clinic", "Blood pressure control and review", "Hypertension", 90),
            ("MCH", "Maternal and Child Health", "Antenatal, postnatal and child welfare", "Pregnancy and infancy", 28),
            ("PALL", "Palliative Care", "Symptom control in advanced disease", "Advanced malignancy", 30),
            ("MH", "Mental Health Clinic", "Follow-up for mental health conditions", "Mental disorders", 60),
        ]
        for i, (code, name, desc, target, interval) in enumerate(programs, start=1):
            self.enrollments_programs = getattr(self, "enrollments_programs", [])
            self.enrollments_programs.append((i, code, name, desc, target, interval, True))

        program_for_icd = {"B20": 1, "A15.0": 2, "E11.9": 3, "E11.2": 3, "I10": 4,
                           "O80": 5, "O82": 5, "O14.9": 5, "C50.9": 6, "C34.9": 6,
                           "F32.1": 7, "F41.1": 7}
        enrolled: set[tuple] = set()
        for event in self.encounter_index:
            program_id = program_for_icd.get(event["icd"])
            if not program_id:
                continue
            key = (event["patient"]["id"], program_id)
            if key in enrolled:
                continue
            enrolled.add(key)
            enrolled_on = event["day"]
            interval = programs[program_id - 1][4]
            status = rng.choices(["active", "lost_to_followup", "transferred_out", "completed", "died", "stopped"],
                                 [64, 12, 8, 10, 3, 3])[0]
            last_visit = min(TODAY, enrolled_on + dt.timedelta(days=rng.randint(0, 500)))
            self.enrollments.append((
                self.next_id("enrollment"), event["patient"]["id"], program_id,
                f"{programs[program_id - 1][0]}-{self.ids['enrollment']:05d}", enrolled_on,
                event["doctor"]["id"], status, last_visit,
                last_visit + dt.timedelta(days=interval) if status == "active" else None,
                last_visit if status not in ("active",) else None,
                {"lost_to_followup": "No contact for over 90 days",
                 "transferred_out": "Transferred to another facility",
                 "completed": "Treatment course completed",
                 "died": "Patient died", "stopped": "Stopped by clinician"}.get(status),
                None))

    def gen_equipment(self) -> None:
        rng = self.rng
        catalogue = [
            ("Patient monitor", "Monitoring", "Mindray", "uMEC12"),
            ("Ventilator", "Critical care", "Draeger", "Savina 300"),
            ("Infusion pump", "Therapy", "B.Braun", "Infusomat"),
            ("Defibrillator", "Emergency", "Philips", "HeartStart"),
            ("Ultrasound machine", "Imaging", "Mindray", "DC-70"),
            ("X-Ray unit", "Imaging", "Siemens", "Multix"),
            ("Anaesthetic machine", "Theatre", "Draeger", "Fabius"),
            ("Operating table", "Theatre", "Maquet", "Alphastar"),
            ("Theatre light", "Theatre", "Maquet", "PowerLED"),
            ("Autoclave", "Sterilisation", "Getinge", "HS22"),
            ("Haematology analyser", "Laboratory", "Sysmex", "XN-550"),
            ("Chemistry analyser", "Laboratory", "Roche", "Cobas c111"),
            ("Blood bank refrigerator", "Laboratory", "Haier", "HXC-358"),
            ("Incubator", "Neonatal", "Draeger", "Isolette"),
            ("Phototherapy unit", "Neonatal", "Fanem", "Bilitron"),
            ("Dialysis machine", "Renal", "Fresenius", "4008S"),
            ("ECG machine", "Cardiology", "Schiller", "AT-102"),
            ("Suction unit", "Ward", "Medela", "Basic"),
            ("Oxygen concentrator", "Ward", "Philips", "EverFlo"),
            ("Wheelchair", "Ward", "Karma", "S-Ergo"),
        ]
        ward_ids = list(self.ward_id.values())
        for _ in range(int(self.n_patients * 0.35)):
            name, category, manufacturer, model = self.pick(catalogue)
            eid = self.next_id("equip")
            purchase = TODAY - dt.timedelta(days=rng.randint(200, 3600))
            status = rng.choices(["in_service", "under_repair", "standby", "awaiting_parts", "decommissioned"],
                                 [74, 9, 9, 5, 3])[0]
            last_service = TODAY - dt.timedelta(days=rng.randint(10, 500))
            dept_code = {"Imaging": "RAD", "Laboratory": "LAB", "Theatre": "SURG",
                         "Critical care": "ICU", "Neonatal": "PAEDS", "Renal": "RENAL",
                         "Cardiology": "CARD", "Emergency": "EMERG"}.get(category, "GENMED")
            self.equipment.append((
                eid, f"AST{eid:05d}", name, category, manufacturer, model,
                f"SN{rng.randint(100000, 999999)}", self.dept_id[dept_code],
                self.pick(ward_ids) if self.chance(0.5) else None, purchase,
                round(rng.uniform(45_000, 4_500_000), 2),
                purchase + dt.timedelta(days=rng.choice([365, 730, 1095])), status,
                last_service, last_service + dt.timedelta(days=rng.choice([90, 180, 365]))))
            for _ in range(rng.randint(1, 4)):
                kind = rng.choices(["preventive", "corrective", "calibration", "inspection"],
                                   [44, 32, 14, 10])[0]
                self.equipment_maint.append((
                    self.next_id("equip_maint"), eid, kind,
                    TODAY - dt.timedelta(days=rng.randint(5, 900)),
                    self.pick(["Biomedical Engineering Ltd", "Medisel Kenya", "In-house workshop",
                               "Manufacturer service centre"]),
                    f"{self.pick(FIRST_M + FIRST_F)} {self.pick(SURNAMES)}",
                    round(rng.uniform(0, 96), 1), round(rng.uniform(0, 180_000), 2),
                    rng.choices(["resolved", "pending_parts", "replaced", "no_fault_found", "condemned"],
                                [66, 14, 9, 8, 3])[0],
                    self.pick(["Routine service completed.", "Awaiting spare part from supplier.",
                               "Calibration certificate issued.", None])))

    def gen_public_health(self) -> None:
        rng = self.rng
        notifiable_codes = {c[0] for c in ICD10 if c[4]}
        for event in self.encounter_index:
            if event["icd"] not in notifiable_codes:
                continue
            if not self.chance(0.8):
                continue
            detected = event["day"]
            classification = rng.choices(["confirmed", "probable", "suspected", "discarded"],
                                         [62, 20, 14, 4])[0]
            reported = detected + dt.timedelta(days=rng.randint(0, 5)) if self.chance(0.85) else None
            self.notifiable.append((
                self.next_id("notifiable"), event["patient"]["id"], event["enc_id"], None,
                event["icd"], event["dx"], classification, detected, reported,
                event["doctor"]["id"] if reported else None,
                self.pick(["County Health Records Officer", "Sub-County Disease Surveillance Coordinator",
                           "National Public Health Laboratory"]) if reported else None,
                classification == "confirmed" and self.chance(0.85),
                self.chance(0.55),
                rng.choices(["recovered", "under_treatment", "died", "transferred", "unknown"],
                            [52, 30, 5, 8, 5])[0],
                None))

    def gen_feedback(self) -> None:
        rng = self.rng
        for event in self.encounter_index:
            if not self.chance(0.4):
                continue
            overall = rng.choices([5, 4, 3, 2, 1], [34, 32, 18, 10, 6])[0]
            waiting = max(1, min(5, overall + rng.choice([-2, -1, -1, 0])))
            comments = {
                5: ["Very good service, staff were kind.", "The doctor explained everything clearly.",
                    "Clean facility and short wait."],
                4: ["Good service overall, the queue was a bit long.", "Satisfied with the treatment given."],
                3: ["Average experience. Waited long before being seen.", "Service was fine but the pharmacy queue was slow."],
                2: ["Waited over three hours in the queue.", "Drugs were out of stock and I had to buy outside."],
                1: ["Very long wait and rude reception staff.", "Nobody explained what was happening to me."],
            }[overall]
            self.feedback.append((
                self.next_id("feedback"), event["patient"]["id"], event["enc_id"],
                self.dept_id[event["dept_code"]],
                event["day"] + dt.timedelta(days=rng.randint(0, 5)),
                rng.choices(["exit_survey", "sms", "suggestion_box", "online", "phone_call"],
                            [44, 26, 14, 10, 6])[0],
                overall, waiting, max(1, min(5, overall + rng.choice([0, 0, 1]))),
                max(1, min(5, overall + rng.choice([-1, 0, 1]))), overall >= 4,
                self.pick(comments), overall <= 2,
                event["day"] + dt.timedelta(days=rng.randint(3, 20)) if overall <= 2 and self.chance(0.6) else None))

    def gen_incidents(self) -> None:
        rng = self.rng
        ward_ids = list(self.ward_id.values())
        for _ in range(int(self.n_patients * 0.45)):
            event = self.pick(self.encounter_index)
            category = rng.choices(
                ["medication_error", "patient_fall", "pressure_ulcer", "needlestick",
                 "equipment_failure", "documentation", "aggression", "delay_in_care",
                 "wrong_site", "infection", "near_miss", "other"],
                [18, 14, 8, 7, 10, 8, 5, 12, 2, 7, 7, 2])[0]
            severity = rng.choices(["no_harm", "low", "moderate", "severe", "death"],
                                   [40, 30, 20, 8, 2])[0]
            occurred = event["when"] + dt.timedelta(hours=rng.randint(1, 72))
            reported = occurred + dt.timedelta(hours=rng.randint(1, 48))
            status = rng.choices(["closed", "under_review", "open"], [58, 26, 16])[0]
            self.incidents.append((
                self.next_id("incident"), f"INC{self.ids['incident']:06d}",
                event["patient"]["id"], event["enc_id"], self.pick(ward_ids),
                self.dept_id[event["dept_code"]], self.pick(self.nurses)["id"],
                occurred, reported, category, severity,
                {"medication_error": "Wrong dose of a regular medicine was prepared and identified before administration.",
                 "patient_fall": "Patient found on the floor beside the bed while mobilising unaided.",
                 "pressure_ulcer": "Grade 2 pressure area noted over the sacrum during routine turning.",
                 "needlestick": "Staff member sustained a needlestick injury while recapping a needle.",
                 "equipment_failure": "Infusion pump alarmed repeatedly and was withdrawn from use.",
                 "documentation": "Observations were not charted for one shift.",
                 "aggression": "Verbal aggression towards nursing staff by a visitor.",
                 "delay_in_care": "Delay in reviewing a deteriorating patient out of hours.",
                 "wrong_site": "Site marking not completed before the procedure; corrected before start.",
                 "infection": "Surgical site infection identified on day five post-operatively.",
                 "near_miss": "Near miss: two patients with similar names almost given each other's medicine.",
                 "other": "Incident recorded for review by the quality team."}[category],
                self.pick(["Patient reviewed immediately and observations increased.",
                           "Item removed from use and reported to biomedical engineering.",
                           "Incident escalated to the nurse in charge.",
                           "Staff member sent to occupational health."]),
                status,
                self.pick(["Staffing levels below the planned establishment on the shift.",
                           "Look-alike packaging in the ward stock.",
                           "Communication gap at handover.",
                           "Equipment overdue for preventive maintenance."]) if status == "closed" else None,
                self.pick(["Ward teaching delivered; process updated.",
                           "Storage of look-alike drugs separated.",
                           "Handover checklist introduced.",
                           "Maintenance schedule revised."]) if status == "closed" else None,
                reported + dt.timedelta(days=rng.randint(3, 60)) if status == "closed" else None))

    def gen_access_log(self) -> None:
        rng = self.rng
        modules = ["patient_summary", "results", "prescriptions", "imaging", "notes",
                   "billing", "admissions", "appointments"]
        for event in self.encounter_index:
            for _ in range(rng.randint(1, 4)):
                staff = self.pick([event["doctor"], event["attending"],
                                   self.pick(self.nurses), self.pick(self.clerks)])
                break_glass = self.chance(0.015)
                self.access_log.append((
                    event["patient"]["id"], staff["id"], event["enc_id"],
                    event["when"] + dt.timedelta(minutes=rng.randint(-30, 2880)),
                    rng.choices(["view", "edit", "print", "search", "export"], [62, 22, 8, 6, 2])[0],
                    self.pick(modules),
                    "Emergency access outside the care team" if break_glass else
                    self.pick(["Direct care", "Ward round", "Results review", "Billing query", None]),
                    f"10.20.{rng.randint(1, 40)}.{rng.randint(2, 250)}", break_glass))

    def next_id(self, key: str) -> int:
        self.ids[key] += 1
        return self.ids[key]

    def build_patient_journey(self, patient: dict) -> None:
        rng = self.rng
        # Each patient carries 1–3 condition profiles that drive their visits.
        profiles = rng.sample(CONDITIONS, rng.choices([1, 2, 3], [46, 38, 16])[0])
        if patient["age"] < 15:
            paeds = [c for c in CONDITIONS if c[1] in ("PAEDS", "GENMED", "ENT") and c[8] != "maternity"]
            profiles = rng.sample(paeds, min(len(paeds), rng.choices([1, 2], [70, 30])[0]))
        if patient["gender"] == "male":
            profiles = [c for c in profiles if c[8] != "maternity"] or [CONDITIONS[0]]

        n_visits = rng.choices([1, 2, 3, 4, 5, 6, 7], [16, 22, 21, 15, 12, 8, 6])[0]
        start_day = max(patient["registered"].date(), HISTORY_START)
        span_days = max(30, (TODAY - start_day).days)
        visit_days = sorted(start_day + dt.timedelta(days=rng.randint(0, span_days)) for _ in range(n_visits))

        for index, day in enumerate(visit_days):
            if patient["deceased"] and day > TODAY - dt.timedelta(days=30):
                break
            profile = self.pick(profiles)
            self.build_encounter(patient, profile, day, is_first=(index == 0))

    # ---- one encounter and everything hanging off it -----------------------

    def build_encounter(self, patient: dict, profile: tuple, day: dt.date, is_first: bool,
                        force_admit: bool = False, force_open: bool = False) -> None:
        rng = self.rng
        (icd, dept_code, complaints, med_names, panels, imaging, chronic, admit_rate, severity) = profile
        dx_desc = next(x[1] for x in ICD10 if x[0] == icd)
        complaint = self.pick(complaints)

        emergency = severity in ("severe", "surgical") and self.chance(0.55)
        if severity == "maternity":
            emergency = self.chance(0.6)
        dept = "EMERG" if emergency else dept_code
        doctor = self.doctor_for(dept)

        # --- the booking (or lack of one) ---------------------------------
        appt_id = None
        appointment_row = None
        if not emergency and self.chance(0.82):
            appt_id = self.uid()
            scheds = self.sched_by_provider.get(doctor["id"]) or []
            hour, minute = (9, 0)
            if scheds:
                sched = self.pick(scheds)
                day = day + dt.timedelta(days=(sched["weekday"] - 1 - day.weekday()) % 7)
                if day > TODAY:
                    day -= dt.timedelta(days=7)
                hour = sched["start"].hour
                minute = rng.randrange(0, 60, sched["slot"] % 60 or 15)
            start = at(day, hour, minute)
            booked_at = start - dt.timedelta(days=rng.randint(1, 30), hours=rng.randint(0, 8))
            appointment_row = {
                "id": appt_id, "start": start, "end": start + dt.timedelta(minutes=20),
                "booked_at": booked_at, "provider": doctor, "dept": dept,
                "sched": self.sched_by_provider.get(doctor["id"], [{}])[0].get("id") if scheds else None,
            }

        arrival_hour = rng.randint(0, 23) if emergency else rng.randint(8, 16)
        encounter_time = at(day, arrival_hour, rng.randint(0, 59))
        enc_id = self.uid()
        enc_no = f"ENC{self.next_id('enc'):06d}"

        # --- admission decision -------------------------------------------
        admitted = force_admit or self.chance(admit_rate)
        still_admitted = force_open or (
            admitted and day > TODAY - dt.timedelta(days=12) and self.chance(0.45))
        enc_type = ("emergency" if emergency else
                    "inpatient" if admitted else
                    "telehealth" if self.chance(0.08) and severity in ("chronic", "minor") else
                    "follow_up" if not is_first and self.chance(0.45) else
                    "daycase" if severity == "surgical" and self.chance(0.3) else
                    "outpatient")

        died = (not force_open and patient["deceased"]
                and severity in ("severe", "surgical") and self.chance(0.25))
        disposition = ("died" if died else "admitted" if admitted else
                       "referred_out" if self.chance(0.04) else "discharged_home")

        attending = self.pick(self.doctors_by_dept.get(dept_code, self.doctors))
        dept_name = next(d[1] for d in DEPARTMENTS if d[0] == dept)

        discharge_time = None
        if not admitted:
            discharge_time = encounter_time + dt.timedelta(minutes=rng.randint(25, 200))

        self.encounters.append((
            enc_id, enc_no, patient["id"], appt_id, encounter_time, enc_type, dept_name,
            self.dept_id[dept], complaint, doctor["id"], attending["id"],
            None, None, None,
            ("ambulance" if emergency and self.chance(0.4) else
             "referral" if self.chance(0.1) else "walk_in"),
            "completed", disposition, discharge_time,
            self.discharge_note(dx_desc, disposition) if not admitted else None,
            (day + dt.timedelta(days=rng.choice([7, 14, 30, 60, 90]))) if self.chance(0.5) else None,
            encounter_time))

        # --- appointment row, now that the encounter exists ---------------
        if appointment_row:
            self.emit_appointment(patient, appointment_row, enc_id, encounter_time, complaint)

        # --- triage (emergency only) --------------------------------------
        if emergency:
            self.emit_triage(patient, enc_id, encounter_time, complaint, severity)

        # --- observations --------------------------------------------------
        self.emit_vitals(patient, enc_id, encounter_time, severity, icd)

        # --- diagnoses -----------------------------------------------------
        self.ids["dx"] += 1
        self.diagnoses.append((self.ids["dx"], enc_id, patient["id"], icd, dx_desc, "primary",
                               doctor["id"], encounter_time + dt.timedelta(minutes=25),
                               "confirmed" if self.chance(0.75) else "probable", not chronic or self.chance(0.8),
                               None, None))
        for _ in range(rng.choices([0, 1, 2], [52, 34, 14])[0]):
            other = self.pick(ICD10)
            self.ids["dx"] += 1
            self.diagnoses.append((self.ids["dx"], enc_id, patient["id"], other[0], other[1],
                                   self.pick(["secondary", "differential"]), doctor["id"],
                                   encounter_time + dt.timedelta(minutes=30),
                                   self.pick(["probable", "suspected", "ruled_out"]), True, None, None))

        # --- consultation note ----------------------------------------------
        self.emit_note(patient, enc_id, doctor, encounter_time, "consultation", complaint, dx_desc, severity)

        # --- orders ---------------------------------------------------------
        lab_cost = self.emit_labs(patient, enc_id, doctor, encounter_time, panels, severity)
        img_cost = self.emit_imaging(patient, enc_id, doctor, encounter_time, imaging)
        rx_cost, rx_item_ids = self.emit_prescription(patient, enc_id, doctor, encounter_time, med_names, admitted)

        # --- procedures and surgery -----------------------------------------
        proc_cost, theatre_cost = self.emit_procedures(patient, enc_id, doctor, encounter_time, severity, dept_code, admitted)

        # --- admission -------------------------------------------------------
        bed_days = 0
        admission_id = None
        if admitted:
            admission_id, bed_days, discharge_time = self.emit_admission(
                patient, enc_id, doctor, attending, encounter_time, dept_code, severity,
                dx_desc, still_admitted, died, rx_item_ids)

        # Index the encounter so the ancillary services (maternity, mortuary,
        # blood bank, programmes, audit) can attach to the right episode.
        self.encounter_index.append({
            "patient": patient, "enc_id": enc_id, "admission_id": admission_id,
            "when": encounter_time, "day": day, "dept": dept, "dept_code": dept_code,
            "icd": icd, "dx": dx_desc, "severity": severity, "doctor": doctor,
            "attending": attending, "admitted": admitted, "died": died,
        })
        if icd in ("O80", "O82") and patient["gender"] == "female":
            self.maternity_events.append({
                "patient": patient, "enc_id": enc_id, "admission_id": admission_id,
                "when": encounter_time, "icd": icd, "doctor": doctor,
            })
        if died:
            self.death_events.append({
                "patient": patient, "enc_id": enc_id, "admission_id": admission_id,
                "when": discharge_time or encounter_time, "icd": icd,
                "doctor": attending, "dept_code": dept_code, "severity": severity,
            })

        # --- referrals --------------------------------------------------------
        if self.chance(0.16):
            self.emit_referral(patient, enc_id, doctor, dept_code, day, dx_desc)

        # --- alerts -----------------------------------------------------------
        self.emit_alerts(patient, enc_id, doctor, encounter_time, med_names)

        # --- billing ----------------------------------------------------------
        self.emit_billing(patient, enc_id, doctor, day, enc_type,
                          lab_cost, img_cost, rx_cost, proc_cost, theatre_cost, bed_days)

    # ---- component emitters -------------------------------------------------

    def emit_appointment(self, patient, appt, enc_id, encounter_time, reason) -> None:
        rng = self.rng
        wait = rng.randint(5, 95)
        self.appointments.append((
            appt["id"], f"APT{self.next_id('appt'):06d}", patient["id"], appt["provider"]["id"],
            self.dept_id[appt["dept"]], appt["sched"], appt["start"], appt["end"],
            self.pick(["new_visit", "follow_up", "review", "procedure", "telehealth", "antenatal", "vaccination", "counselling"]),
            "completed",
            rng.choices(["front_desk", "phone", "online", "walk_in", "referral"], [40, 22, 18, 12, 8])[0],
            appt["booked_at"], self.pick(self.clerks)["id"], reason,
            appt["start"] - dt.timedelta(minutes=rng.randint(5, 40)),
            encounter_time, wait, enc_id, None, None, None, None))

    def future_appointment(self, patient: dict) -> None:
        rng = self.rng
        doctor = self.pick(self.doctors)
        scheds = self.sched_by_provider.get(doctor["id"])
        if not scheds:
            return
        sched = self.pick(scheds)
        offset = rng.randint(1, 42)
        day = TODAY + dt.timedelta(days=offset)
        day += dt.timedelta(days=(sched["weekday"] - 1 - day.weekday()) % 7)
        start = at(day, sched["start"].hour, rng.randrange(0, 60, 15))
        booked = at(TODAY - dt.timedelta(days=rng.randint(1, 40)), rng.randint(8, 16), 0)
        status = rng.choices(["booked", "confirmed", "cancelled", "rescheduled"], [58, 30, 8, 4])[0]
        cancelled_at = booked + dt.timedelta(days=1) if status == "cancelled" else None
        self.appointments.append((
            self.uid(), f"APT{self.next_id('appt'):06d}", patient["id"], doctor["id"],
            self.dept_id[doctor["dept"]], sched["id"], start,
            start + dt.timedelta(minutes=sched["slot"]),
            self.pick(["new_visit", "follow_up", "review", "procedure", "antenatal", "vaccination"]),
            status,
            rng.choices(["front_desk", "phone", "online", "referral"], [38, 26, 26, 10])[0],
            booked, self.pick(self.clerks)["id"],
            self.pick(["Routine review", "Results review", "Chronic care follow-up",
                       "Post-operative check", "Specialist opinion", "Antenatal visit"]),
            None, None, None, None, cancelled_at,
            self.pick(["Patient request", "Clinician unavailable", "Duplicate booking"]) if cancelled_at else None,
            None, None))

    def emit_triage(self, patient, enc_id, arrival, complaint, severity) -> None:
        rng = self.rng
        category = {"severe": rng.choice([1, 2]), "surgical": rng.choice([2, 3]),
                    "acute": rng.choice([3, 3, 4]), "maternity": rng.choice([2, 3]),
                    "chronic": rng.choice([3, 4]), "minor": rng.choice([4, 5])}[severity]
        colour = {1: "red", 2: "orange", 3: "yellow", 4: "green", 5: "blue"}[category]
        to_triage = rng.randint(2, 25)
        to_doctor = {1: rng.randint(0, 5), 2: rng.randint(5, 25), 3: rng.randint(20, 70),
                     4: rng.randint(40, 150), 5: rng.randint(60, 240)}[category]
        nurse = self.nurse_for("EMERG")
        self.triage.append((
            self.next_id("triage"), enc_id, patient["id"], nurse["id"], arrival,
            arrival + dt.timedelta(minutes=to_triage),
            arrival + dt.timedelta(minutes=to_triage + to_doctor),
            category, colour, complaint, to_triage, to_triage + to_doctor,
            self.pick(["Brought in by relatives.", "Self-presenting.", "Referred from a health centre.",
                       "Arrived by ambulance.", None])))

    def emit_vitals(self, patient, enc_id, when, severity, icd) -> None:
        rng = self.rng
        age = patient["age"]
        if age < 1:
            height, weight = rng.uniform(48, 76), rng.uniform(3.0, 9.5)
            pulse, resp = rng.randint(110, 160), rng.randint(30, 55)
        elif age < 12:
            height, weight = rng.uniform(80, 150), rng.uniform(11, 44)
            pulse, resp = rng.randint(80, 130), rng.randint(18, 30)
        else:
            height = rng.uniform(150, 186) if patient["gender"] == "male" else rng.uniform(145, 175)
            weight = rng.uniform(45, 105)
            pulse, resp = rng.randint(58, 100), rng.randint(12, 20)

        systolic = rng.randint(100, 128)
        if icd in ("I10", "I25.1", "I50.9", "N18.3", "N18.5", "O14.9") or age > 60:
            systolic = rng.randint(132, 186)
        diastolic = systolic - rng.randint(35, 55)
        temp = round(rng.uniform(36.2, 37.4), 1)
        if severity in ("acute", "severe") or icd in ("B50.9", "J18.9", "A41.9", "A01.0", "L03.9"):
            temp = round(rng.uniform(37.8, 40.1), 1)
            pulse += rng.randint(10, 35)
        spo2 = round(rng.uniform(96, 100), 1)
        if icd in ("J45.9", "J44.9", "J18.9", "I50.9", "A41.9"):
            spo2 = round(rng.uniform(85, 95), 1)
        glucose = round(rng.uniform(4.0, 6.4), 2)
        if icd in ("E11.9", "E11.2"):
            glucose = round(rng.uniform(7.2, 19.5), 2)
        gcs = 15 if severity != "severe" else rng.choice([15, 15, 14, 13, 11, 9])
        pain = {"severe": rng.randint(6, 10), "surgical": rng.randint(5, 9),
                "acute": rng.randint(3, 7), "maternity": rng.randint(6, 10),
                "chronic": rng.randint(1, 5), "minor": rng.randint(0, 4)}[severity]

        news2 = 0
        news2 += 3 if spo2 < 92 else (2 if spo2 < 94 else (1 if spo2 < 96 else 0))
        news2 += 3 if temp >= 39.1 else (1 if temp >= 38.1 or temp <= 36.0 else 0)
        news2 += 3 if pulse >= 131 else (2 if pulse >= 111 else (1 if pulse >= 91 else 0))
        news2 += 3 if systolic <= 90 else (1 if systolic >= 180 else 0)
        news2 += 3 if gcs < 15 else 0

        nurse = self.pick(self.nurses)
        self.vitals.append((
            self.next_id("vital"), enc_id, patient["id"], nurse["id"], temp, pulse, resp,
            systolic, diastolic, spo2, round(weight, 1), round(height, 1), pain, gcs, glucose,
            news2, when + dt.timedelta(minutes=rng.randint(3, 20))))

    def emit_note(self, patient, enc_id, author, when, note_type, complaint, dx_desc, severity) -> None:
        rng = self.rng
        duration = self.pick(["2 days", "3 days", "1 week", "2 weeks", "since yesterday", "1 month"])
        subjective = (f"{patient['age']}-year-old {patient['gender']} presenting with {complaint.lower()} "
                      f"for {duration}. " + self.pick([
                          "No vomiting or diarrhoea reported.",
                          "Appetite reduced; fluid intake maintained.",
                          "Denies chest pain or palpitations.",
                          "Reports good adherence to current medication.",
                          "Has been self-medicating with over-the-counter analgesia.",
                          "No known contact with a sick person."]))
        objective = self.pick([
            "Alert and oriented. Chest clear on auscultation, heart sounds normal.",
            "Mildly dehydrated. Abdomen soft, tender in the epigastrium, no guarding.",
            "Febrile and tachycardic. Peripheries warm, capillary refill under 2 seconds.",
            "Pale conjunctivae. No jaundice, no lymphadenopathy, no pedal oedema.",
            "Reduced air entry at the right base with coarse crepitations.",
            "Bilateral pedal oedema to mid-shin. Raised jugular venous pressure."])
        assessment = f"{dx_desc}. " + self.pick([
            "Clinically stable for outpatient management.",
            "Requires admission for observation and intravenous therapy.",
            "Consistent with the working diagnosis; awaiting investigations.",
            "Chronic condition, currently suboptimally controlled.",
            "Improving on the current regimen."])
        plan = self.pick([
            "Start treatment as charted, review with results.",
            "Admit, commence intravenous fluids and antibiotics, monitor observations 4-hourly.",
            "Analgesia, hydration advice, review in one week or earlier if worse.",
            "Continue current therapy, reinforce adherence, review in the clinic in one month.",
            "Refer for specialist opinion and further imaging.",
            "Discharge with medication, safety-net advice given to the patient and relative."])
        body = f"S: {subjective}\nO: {objective}\nA: {assessment}\nP: {plan}"
        self.notes.append((
            self.next_id("note"), enc_id, patient["id"], author["id"], note_type,
            when + dt.timedelta(minutes=rng.randint(20, 60)),
            subjective, objective, assessment, plan, body, True,
            when + dt.timedelta(minutes=rng.randint(60, 120))))

    def emit_labs(self, patient, enc_id, doctor, when, panels, severity) -> float:
        rng = self.rng
        total = 0.0
        for panel in panels:
            tests = self.lab_tests_by_panel.get(panel)
            if not tests:
                continue
            order_id = self.uid()
            priority = ("stat" if severity == "severe" else
                        "urgent" if severity in ("acute", "surgical") and self.chance(0.5) else "routine")
            status = "resulted" if self.chance(0.93) else self.pick(["ordered", "collected", "processing", "cancelled"])
            collected = when + dt.timedelta(minutes=rng.randint(10, 90))
            tech = self.pick(self.lab_techs)
            price = self.panel_price.get(panel, 800)
            total += price if status == "resulted" else 0
            self.lab_orders.append((
                order_id, f"LAB{self.next_id('lab'):06d}", enc_id, patient["id"], doctor["id"],
                tech["id"] if status != "ordered" else None, when + dt.timedelta(minutes=rng.randint(5, 40)),
                collected if status != "ordered" else None, panel, priority, status,
                tests[0][2], price))
            if status != "resulted":
                continue
            tat = tests[0][7] or 4
            resulted_at = collected + dt.timedelta(hours=tat, minutes=rng.randint(0, 120))
            for panel_name, test_name, spec, unit, low, high, ref_text, _, _ in tests:
                abnormal = self.chance(0.28 if severity in ("severe", "acute", "surgical") else 0.16)
                if low is not None and high is not None:
                    if abnormal:
                        if self.chance(0.5):
                            value = round(rng.uniform(float(low) * 0.35, float(low) * 0.92), 2)
                            flag = "LL" if value < float(low) * 0.6 else "L"
                        else:
                            value = round(rng.uniform(float(high) * 1.08, float(high) * 2.4), 2)
                            flag = "HH" if value > float(high) * 1.8 else "H"
                    else:
                        value = round(rng.uniform(float(low), float(high)), 2)
                        flag = None
                    critical = flag in ("LL", "HH") and self.chance(0.5)
                    self.lab_results.append((
                        self.next_id("lab_res"), order_id, patient["id"], test_name, str(value), value,
                        unit, f"{low} - {high}", flag is not None, flag, critical,
                        tech["id"], resulted_at,
                        "Repeat sample requested." if critical and self.chance(0.4) else None))
                else:
                    positive = abnormal
                    text = {"Negative": "Positive", "Non-reactive": "Reactive",
                            "No growth": "Growth detected", "No ova or cysts": "Cysts seen",
                            "Not detected": "MTB detected", "Reported": patient["blood"]}.get(ref_text, "Abnormal")
                    value_text = text if positive else (ref_text or "Normal")
                    if ref_text == "Reported":
                        value_text, positive = patient["blood"], False
                    self.lab_results.append((
                        self.next_id("lab_res"), order_id, patient["id"], test_name, value_text, None,
                        unit, ref_text, positive, None, positive and self.chance(0.3),
                        tech["id"], resulted_at, None))
        return total

    def emit_imaging(self, patient, enc_id, doctor, when, imaging) -> float:
        if not imaging or not self.chance(0.75):
            return 0.0
        rng = self.rng
        modality, body_part = imaging
        price = {"X-Ray": 2500, "Ultrasound": 3500, "CT": 12000, "MRI": 22000,
                 "Echocardiogram": 6500, "Mammography": 5500}.get(modality, 4000)
        status = "resulted" if self.chance(0.85) else self.pick(["ordered", "scheduled", "performed"])
        performed = when + dt.timedelta(hours=rng.randint(1, 30)) if status in ("performed", "resulted") else None
        radiologist = self.pick(self.radiologists)
        abnormal = self.chance(0.45)
        findings_normal = {
            "Chest": "Lung fields are clear. Cardiac silhouette within normal limits. No pleural effusion.",
            "Brain": "No intracranial haemorrhage or mass effect. Ventricles are of normal size.",
            "Abdomen": "Liver, spleen and kidneys are of normal size and echotexture. No free fluid.",
            "Renal": "Both kidneys are of normal size with preserved corticomedullary differentiation.",
            "Heart": "Good biventricular function. No regional wall motion abnormality. EF 60%.",
            "Both Breasts": "Scattered fibroglandular density. No suspicious mass or microcalcification.",
        }
        findings_abnormal = {
            "Chest": "Patchy consolidation in the right lower zone with an air bronchogram.",
            "Brain": "Hypodense area in the left middle cerebral artery territory, consistent with infarction.",
            "Abdomen": "Dilated non-compressible blind-ending tubular structure in the right iliac fossa.",
            "Renal": "Bilateral increased cortical echogenicity with reduced corticomedullary differentiation.",
            "Heart": "Dilated left ventricle with global hypokinesia. EF estimated at 35%.",
            "Both Breasts": "Spiculated mass in the upper outer quadrant of the left breast, BI-RADS 5.",
        }
        default_normal = "No acute abnormality demonstrated."
        default_abnormal = "Findings consistent with the clinical indication; correlate clinically."
        findings = (findings_abnormal.get(body_part, default_abnormal) if abnormal
                    else findings_normal.get(body_part, default_normal))
        impression = ("Abnormal study — see findings. Recommend clinical correlation and follow-up."
                      if abnormal else "Normal study.")
        self.imaging.append((
            self.uid(), f"IMG{self.next_id('img'):06d}", enc_id, patient["id"], doctor["id"],
            modality, body_part,
            self.pick(["Rule out acute pathology.", "Assess disease extent.", "Pre-operative assessment.",
                       "Persistent symptoms despite treatment.", "Routine surveillance."]),
            "urgent" if self.chance(0.25) else "routine",
            when + dt.timedelta(minutes=rng.randint(10, 60)), performed, status,
            findings if status == "resulted" else None,
            impression if status == "resulted" else None,
            f"{findings} {impression}" if status == "resulted" else None,
            abnormal if status == "resulted" else None,
            radiologist["id"] if status == "resulted" else None,
            performed + dt.timedelta(hours=rng.randint(1, 12)) if status == "resulted" else None,
            price))
        return price if status in ("performed", "resulted") else 0.0

    def emit_prescription(self, patient, enc_id, doctor, when, med_names, admitted):
        rng = self.rng
        if not med_names or not self.chance(0.9):
            return 0.0, []
        rx_id = self.uid()
        issue = when.date()
        pharmacist = self.pick(self.pharmacists)
        status = rng.choices(["dispensed", "active", "partially_dispensed", "cancelled", "expired"],
                             [66, 14, 8, 4, 8])[0]
        if issue < TODAY - dt.timedelta(days=120) and status == "active":
            status = "expired"
        dispensed_at = when + dt.timedelta(hours=rng.randint(1, 6)) if status in ("dispensed", "partially_dispensed") else None
        self.prescriptions.append((
            rx_id, f"RX{self.next_id('rx'):06d}", enc_id, patient["id"], doctor["id"], issue,
            issue + dt.timedelta(days=rng.choice([14, 30, 60, 90])), status,
            pharmacist["id"] if dispensed_at else None, dispensed_at,
            self.pick(["Counselled on adherence.", "Advised to complete the full course.",
                       "Take after food.", None]),
            when))

        total, item_ids = 0.0, []
        for name in med_names:
            med_id = self.med_id.get(name)
            if med_id is None:
                continue
            med = MEDICATIONS[med_id - 1]
            qty = rng.choice([6, 10, 14, 15, 20, 21, 28, 30, 60])
            unit_price = float(med[8])
            line = round(unit_price * qty, 2)
            total += line
            item_dispensed = status == "dispensed" or (status == "partially_dispensed" and self.chance(0.5))
            item_id = self.next_id("rx_item")
            item_ids.append((item_id, name, med[7]))
            self.rx_items.append((
                item_id, rx_id, med_id, med[5],
                self.pick(["Once daily", "Twice daily", "Three times daily", "Every 8 hours",
                           "Every 6 hours", "At night", "As needed"]),
                rng.choice([3, 5, 7, 10, 14, 30]), qty, med[7],
                self.pick(["Take after food.", "Swallow whole with water.", "Complete the full course.",
                           "Use two puffs when breathless.", "Apply thinly to the affected area.", None]),
                item_dispensed, dispensed_at if item_dispensed else None, unit_price, line))
        return total, item_ids

    def emit_procedures(self, patient, enc_id, doctor, when, severity, dept_code, admitted):
        rng = self.rng
        proc_cost = theatre_cost = 0.0
        surgical_map = {
            "K35.8": "PRC011", "K40.9": "PRC012", "O82": "PRC013", "S52.5": "PRC016",
            "S82.6": "PRC015", "H25.9": "PRC017", "C50.9": "PRC020",
        }
        minor_pool = ["PRC001", "PRC002", "PRC003", "PRC004", "PRC005", "PRC006", "PRC007",
                      "PRC008", "PRC009", "PRC010", "PRC023", "PRC024", "PRC025"]

        if severity in ("acute", "severe", "surgical", "maternity") and self.chance(0.55):
            code = self.pick(minor_pool)
            meta = next(p for p in PROCEDURE_CODES if p[0] == code)
            performer = self.pick(self.nurses) if meta[2] == "Nursing" else doctor
            proc_id = self.next_id("proc")
            proc_cost += 1200 if meta[2] == "Nursing" else 4500
            self.procedures.append((
                proc_id, enc_id, patient["id"], code, meta[1], performer["id"],
                when + dt.timedelta(minutes=rng.randint(30, 240)),
                self.pick(["Treatment room", "Ward side room", "Emergency bay", "Clinic room"]),
                "none" if meta[2] == "Nursing" else "local",
                rng.choices(["successful", "partial", "complication"], [88, 8, 4])[0],
                None,
                self.pick(["Tolerated well.", "Patient counselled beforehand.", None])))

        if severity == "surgical" or (severity == "maternity" and self.chance(0.4)):
            code = surgical_map.get(None) or self.pick([c[0] for c in PROCEDURE_CODES if c[3]])
            meta = next(p for p in PROCEDURE_CODES if p[0] == code)
            surgeon = self.pick(self.doctors_by_dept.get("SURG", self.doctors))
            if dept_code == "ORTHO":
                surgeon = self.pick(self.doctors_by_dept.get("ORTHO", self.doctors))
            elif dept_code == "OBGYN":
                surgeon = self.pick(self.doctors_by_dept.get("OBGYN", self.doctors))
            elif dept_code == "OPHTH":
                surgeon = self.pick(self.doctors_by_dept.get("OPHTH", self.doctors))
            proc_id = self.next_id("proc")
            anaesthesia = self.pick(["general", "spinal", "regional", "sedation"])
            scheduled = when + dt.timedelta(hours=rng.randint(2, 48))
            status = rng.choices(["completed", "cancelled", "postponed", "scheduled"], [86, 5, 5, 4])[0]
            actual_start = scheduled + dt.timedelta(minutes=rng.randint(0, 120)) if status == "completed" else None
            minutes = int((meta[4] or 60) * rng.uniform(0.7, 1.9))
            actual_end = actual_start + dt.timedelta(minutes=minutes) if actual_start else None
            outcome = rng.choices(["successful", "complication"], [92, 8])[0] if status == "completed" else "abandoned"
            self.procedures.append((
                proc_id, enc_id, patient["id"], code, meta[1], surgeon["id"],
                actual_start or scheduled, f"Theatre {rng.randint(1, 4)}", anaesthesia,
                outcome if status == "completed" else "abandoned",
                self.pick(["Wound infection on day 3.", "Intra-operative bleeding controlled.",
                           "Prolonged ileus post-operatively."]) if outcome == "complication" else None,
                None))
            theatre_cost += {"completed": 28000, "cancelled": 0, "postponed": 0, "scheduled": 0}[status]
            self.surgeries.append((
                self.next_id("surg"), proc_id, enc_id, patient["id"], f"T{rng.randint(1, 4)}",
                surgeon["id"], self.pick(self.doctors)["id"],
                self.pick(self.anaesthetists)["id"] if self.anaesthetists else None,
                self.pick(self.nurses)["id"],
                "emergency" if severity == "severe" else self.pick(["elective", "urgent"]),
                rng.choices([1, 2, 3, 4], [30, 40, 22, 8])[0], scheduled, actual_start, actual_end,
                rng.choice([50, 100, 150, 250, 400, 800]) if status == "completed" else None,
                status,
                self.pick(["No theatre space", "Patient not fasted", "Equipment failure",
                           "Patient unwell"]) if status in ("cancelled", "postponed") else None,
                self.pick(["Inflamed appendix removed, no perforation.",
                           "Hernia sac dissected and repaired with mesh.",
                           "Live infant delivered, uterus closed in two layers.",
                           "Fracture reduced and fixed with a plate and screws.",
                           "Lens removed and intraocular lens implanted."]) if status == "completed" else None))
        return proc_cost, theatre_cost

    def emit_admission(self, patient, enc_id, doctor, attending, when, dept_code, severity,
                       dx_desc, still_admitted, died, rx_item_ids):
        rng = self.rng
        ward_map = {"EMERG": "ISO", "GENMED": self.pick(["MWA", "MWB"]), "SURG": "SW",
                    "PAEDS": "PW", "OBGYN": self.pick(["MAT", "LAB_W"]), "ORTHO": "ORTHW",
                    "RENAL": "RENW", "PSYCH": "PSYW", "ICU": self.pick(["ICU_W", "HDU"]),
                    "CARD": self.pick(["MWA", "MWB"]), "ONCOL": self.pick(["MWA", "MWB"])}
        ward_code = ward_map.get(dept_code, "MWA")
        if severity == "severe" and self.chance(0.55):
            ward_code = self.pick(["ICU_W", "HDU"])
        ward_id = self.ward_id[ward_code]
        ward_name = next(w[1] for w in WARDS if w[0] == ward_code)

        los = {"severe": rng.randint(3, 21), "surgical": rng.randint(2, 10),
               "acute": rng.randint(1, 7), "maternity": rng.randint(1, 4),
               "chronic": rng.randint(2, 9), "minor": rng.randint(1, 3)}[severity]
        discharge = None if still_admitted else when + dt.timedelta(days=los, hours=rng.randint(0, 10))
        if discharge and discharge > tz(dt.datetime.combine(TODAY, dt.time(23, 0))):
            discharge = None
            still_admitted = True

        prior = [a for a in self.admissions if a[2] == patient["id"] and a[13]]
        readmission = any(0 < (when - a[13]).days <= 30 for a in prior if a[13])

        adm_id = self.next_id("adm")
        bed_id, ward_id = self.free_bed(ward_id)
        ward_name = next(w[1] for w in WARDS if self.ward_id[w[0]] == ward_id)
        ward_code = next(w[0] for w in WARDS if self.ward_id[w[0]] == ward_id)
        if still_admitted:
            self.occupied_beds.add(bed_id)
        discharge_type = None
        if discharge:
            discharge_type = ("deceased" if died else
                              rng.choices(["home", "transfer", "against_advice", "absconded"],
                                          [86, 7, 5, 2])[0])
        self.admissions.append((
            adm_id, enc_id, patient["id"], doctor["id"], attending["id"], when, ward_name, ward_id,
            f"{ward_code}-{rng.randint(1, 20):02d}",
            ("emergency" if severity in ("severe", "acute") else
             "maternity" if severity == "maternity" else
             "daycase" if severity == "surgical" and los <= 1 else "elective"),
            ("emergency_dept" if severity in ("severe", "acute") else
             "maternity" if severity == "maternity" else
             self.pick(["outpatient_clinic", "referral", "theatre"])),
            dx_desc, readmission, discharge, discharge_type,
            self.discharge_summary(dx_desc, discharge_type, los) if discharge else None))

        self.bed_assignments.append((
            self.next_id("bed_assign"), adm_id, bed_id, ward_id, when,
            discharge, self.nurse_for(dept_code)["id"]))

        if los > 4 and self.chance(0.3):
            to_ward = self.pick([w for w in self.ward_id.values() if w != ward_id])
            self.transfers.append((
                self.next_id("transfer"), adm_id, ward_id, to_ward,
                when + dt.timedelta(days=rng.randint(1, max(1, los - 1))),
                self.pick(["Step-down from intensive care", "Bed management",
                           "Specialty change after review", "Isolation no longer required",
                           "Deterioration requiring closer monitoring"]),
                attending["id"]))

        # Inpatient drug rounds.
        for item_id, med_name, route in rx_item_ids:
            rounds = min(los, 7) * rng.choice([1, 2, 3])
            for r in range(rounds):
                scheduled = when + dt.timedelta(hours=8 * (r + 1))
                if discharge and scheduled > discharge:
                    break
                status = rng.choices(["given", "held", "refused", "missed", "self_administered"],
                                     [88, 4, 3, 3, 2])[0]
                self.mar.append((
                    self.next_id("mar"), item_id, adm_id, patient["id"],
                    self.nurse_for(dept_code)["id"] if status != "missed" else None,
                    scheduled,
                    scheduled + dt.timedelta(minutes=rng.randint(-20, 55)) if status != "missed" else None,
                    MEDICATIONS[self.med_id[med_name] - 1][5] if status == "given" else None,
                    route, status,
                    self.pick(["Patient nil by mouth", "Blood pressure too low", "Patient declined",
                               "Drug out of stock", "Patient off ward for imaging"])
                    if status in ("held", "refused", "missed") else None,
                    None))

        # Serial observations: a ward patient is not observed once and forgotten.
        for d in range(1, min(los, 8) + 1):
            for hour in (6, 14, 22):
                obs_time = when + dt.timedelta(days=d, hours=hour - when.hour)
                if discharge and obs_time > discharge:
                    break
                if obs_time > tz(dt.datetime.combine(TODAY, dt.time(23, 59))):
                    break
                if self.chance(0.35):
                    continue
                self.emit_vitals(patient, enc_id, obs_time, severity, "")

        # Ward round notes.
        for d in range(1, min(los, 6) + 1):
            note_time = when + dt.timedelta(days=d, hours=rng.randint(7, 11))
            if discharge and note_time > discharge:
                break
            self.notes.append((
                self.next_id("note"), enc_id, patient["id"],
                (attending if self.chance(0.6) else self.nurse_for(dept_code))["id"],
                "progress" if self.chance(0.7) else "nursing", note_time,
                self.pick(["Slept well, no new complaints.", "Reports less pain overnight.",
                           "Still febrile, appetite poor.", "Mobilising with assistance."]),
                self.pick(["Observations stable. Chest clear.", "Wound site clean and dry.",
                           "Afebrile, passing urine well.", "Mild tenderness on palpation."]),
                self.pick(["Improving as expected.", "Slow but steady progress.",
                           "No new concerns identified.", "Response to treatment is adequate."]),
                self.pick(["Continue current management.", "Repeat bloods in the morning.",
                           "Plan discharge tomorrow if stable.", "Physiotherapy review requested."]),
                f"Day {d} ward round. Patient reviewed, plan continued.", True, note_time))

        if discharge:
            self.notes.append((
                self.next_id("note"), enc_id, patient["id"], attending["id"], "discharge",
                discharge, None, None, None, None,
                self.discharge_summary(dx_desc, discharge_type, los), True, discharge))

        return adm_id, (los if not still_admitted else max(1, (TODAY - when.date()).days)), discharge

    def discharge_note(self, dx_desc: str, disposition: str) -> str:
        if disposition == "referred_out":
            return f"Reviewed for {dx_desc.lower()}. Referred for specialist management; letter issued."
        if disposition == "died":
            return f"Patient deteriorated despite resuscitation. Cause of death: {dx_desc.lower()}."
        return self.pick([
            f"Seen and treated for {dx_desc.lower()}. Discharged with medication and advice.",
            f"Symptomatic treatment given for {dx_desc.lower()}. Safety-net advice provided.",
            f"{dx_desc}. Stable for discharge; follow-up arranged in the outpatient clinic.",
        ])

    def discharge_summary(self, dx_desc: str, discharge_type: str, los: int) -> str:
        if discharge_type == "deceased":
            return (f"Admitted with {dx_desc.lower()}. Deteriorated on the ward despite maximal supportive care. "
                    f"Resuscitation attempted and discontinued after {los} days. Family informed and counselled.")
        if discharge_type == "against_advice":
            return (f"Admitted with {dx_desc.lower()}. Patient elected to leave against medical advice on day {los}. "
                    "Risks explained and documented; open follow-up offered.")
        if discharge_type == "transfer":
            return (f"Admitted with {dx_desc.lower()}. Stabilised over {los} days and transferred for specialist "
                    "care. Transfer summary and investigations sent with the patient.")
        if discharge_type == "absconded":
            return f"Admitted with {dx_desc.lower()}. Patient absconded from the ward on day {los}. Next of kin contacted."
        return self.pick([
            (f"Admitted with {dx_desc.lower()}. Treated with intravenous therapy and supportive care. "
             f"Made steady progress over {los} days and was apyrexial for 48 hours before discharge. "
             "Discharged home on oral medication with clinic review in two weeks."),
            (f"Admitted for management of {dx_desc.lower()}. Investigations completed and treatment commenced. "
             f"Symptoms resolved by day {los}. Discharged in stable condition with written advice."),
            (f"Emergency admission with {dx_desc.lower()}. Responded well to initial management. "
             f"Mobilised independently before discharge on day {los}. Follow-up arranged."),
        ])

    def emit_referral(self, patient, enc_id, doctor, dept_code, day, dx_desc) -> None:
        rng = self.rng
        direction = rng.choices(["internal", "outbound", "inbound"], [72, 20, 8])[0]
        to_dept = None
        to_provider = None
        external = None
        if direction == "internal":
            to_code = self.pick([d[0] for d in DEPARTMENTS if d[2] == "clinical" and d[0] != dept_code])
            to_dept = self.dept_id[to_code]
            pool = self.doctors_by_dept.get(to_code)
            to_provider = self.pick(pool)["id"] if pool else None
        else:
            external = self.pick(["Kenyatta National Hospital", "Moi Teaching and Referral Hospital",
                                  "Aga Khan University Hospital", "Coast General Teaching Hospital",
                                  "Nakuru County Referral Hospital", "MP Shah Hospital"])
        status = rng.choices(["completed", "accepted", "scheduled", "pending", "declined", "expired"],
                             [40, 20, 14, 14, 6, 6])[0]
        responded = day + dt.timedelta(days=rng.randint(1, 30)) if status != "pending" else None
        self.referrals.append((
            self.next_id("referral"), f"REF{self.ids['referral']:06d}", patient["id"], enc_id,
            doctor["id"], self.dept_id[dept_code], to_dept, to_provider, external, direction,
            rng.choices(["routine", "urgent", "stat"], [64, 30, 6])[0],
            self.pick([f"Specialist opinion on {dx_desc.lower()}.",
                       "Further investigation beyond local capacity.",
                       "Ongoing specialist follow-up required.",
                       "Surgical assessment requested.",
                       "Second opinion requested by the family."]),
            f"{dx_desc}. Investigations and treatment to date summarised in the attached notes.",
            status, day, responded,
            self.pick(["Seen and managed; back-referred with a plan.",
                       "Appointment given for the specialist clinic.",
                       "Declined — no bed capacity at the receiving unit.",
                       "Patient did not attend."]) if responded else None))

    def emit_alerts(self, patient, enc_id, doctor, when, med_names) -> None:
        rng = self.rng
        if not self.chance(0.28):
            return
        kind = rng.choices(["drug_interaction", "allergy", "critical_result", "renal_dosing",
                            "duplicate_therapy", "missing_vitals"], [26, 18, 22, 14, 12, 8])[0]
        severity = {"drug_interaction": self.pick(["warning", "critical"]),
                    "allergy": "critical", "critical_result": "critical",
                    "renal_dosing": "warning", "duplicate_therapy": "info",
                    "missing_vitals": "info"}[kind]
        drug1 = drug2 = allergen = None
        if kind == "drug_interaction" and len(med_names) >= 2:
            drug1 = self.med_id.get(med_names[0])
            drug2 = self.med_id.get(med_names[1])
            title = "Potential drug interaction"
            message = ("Two prescribed medicines interact. Review the combination and monitor "
                       "for adverse effects, or select an alternative agent.")
        elif kind == "allergy":
            allergen = self.rng.randint(1, len(ALLERGENS))
            title = "Prescribed drug matches a documented allergy"
            message = "The patient has a recorded allergy to this agent or a cross-reacting class. Verify before administration."
        elif kind == "critical_result":
            title = "Critical laboratory result"
            message = "A result outside the critical limit has been reported. Immediate clinical review is required."
        elif kind == "renal_dosing":
            title = "Dose adjustment required in renal impairment"
            message = "Estimated GFR is reduced. Adjust the dose or interval according to renal function."
        elif kind == "duplicate_therapy":
            title = "Possible duplicate therapy"
            message = "Two agents from the same therapeutic class are active for this patient."
        else:
            title = "Observations overdue"
            message = "No observation set has been recorded for this patient in the last 8 hours."

        acknowledged = self.chance(0.55)
        self.alerts.append((
            self.next_id("alert"), patient["id"], enc_id, kind, severity, title, message,
            drug1, drug2, allergen, when + dt.timedelta(minutes=rng.randint(5, 90)), doctor["id"],
            acknowledged, doctor["id"] if acknowledged else None,
            when + dt.timedelta(minutes=rng.randint(95, 400)) if acknowledged else None,
            self.pick(["Benefit outweighs risk; monitoring in place.", "Alternative not available.",
                       "Reviewed with the pharmacist.", None]) if acknowledged else None))

    def emit_billing(self, patient, enc_id, doctor, day, enc_type,
                     lab_cost, img_cost, rx_cost, proc_cost, theatre_cost, bed_days) -> None:
        rng = self.rng
        bill_id = self.uid()
        items = []
        consult_fee = float(doctor["fee"] or 1500)
        items.append(("consultation", "CONS", f"{doctor['title']} consultation", 1, consult_fee))
        if lab_cost:
            items.append(("lab", "LAB", "Laboratory investigations", 1, lab_cost))
        if img_cost:
            items.append(("imaging", "IMG", "Diagnostic imaging", 1, img_cost))
        if rx_cost:
            items.append(("pharmacy", "PHARM", "Dispensed medication", 1, round(rx_cost, 2)))
        if proc_cost:
            items.append(("procedure", "PROC", "Clinical procedure", 1, proc_cost))
        if theatre_cost:
            items.append(("theatre", "THTR", "Theatre and anaesthesia", 1, theatre_cost))
        if bed_days:
            rate = float(self.pick([2500, 3500, 4500, 8000]))
            items.append(("bed", "BED", "Bed charges", bed_days, rate))
            items.append(("nursing", "NURS", "Nursing care", bed_days, 1200.0))
        if self.chance(0.35):
            items.append(("consumable", "CONS-M", "Consumables and dressings",
                          rng.randint(1, 5), float(self.pick([150, 300, 450, 800]))))

        subtotal = 0.0
        for item_type, code, desc, qty, unit in items:
            line = round(unit * qty, 2)
            subtotal += line
            self.bill_items.append((self.next_id("bill_item"), bill_id, item_type, code, desc, qty, unit, line))
        subtotal = round(subtotal, 2)

        discount = round(subtotal * self.pick([0, 0, 0, 0.05, 0.1]), 2)
        insurance_id = self.insurance_for.get(patient["id"])
        cover = 0.0
        if insurance_id and self.chance(0.88):
            cover = round((subtotal - discount) * self.pick([0.6, 0.7, 0.8, 0.9, 1.0]), 2)
        patient_due = round(subtotal - discount - cover, 2)

        paid = 0.0
        pay_rows = []
        if patient_due > 0:
            if self.chance(0.7):
                paid = patient_due
            elif self.chance(0.6):
                paid = round(patient_due * self.pick([0.3, 0.5, 0.75]), 2)
            if paid > 0:
                remaining = paid
                for _ in range(rng.choices([1, 2], [82, 18])[0]):
                    amount = remaining if remaining <= 0 else round(remaining / (2 if remaining < paid else 1), 2)
                    amount = remaining
                    pay_rows.append(amount)
                    remaining -= amount
                    if remaining <= 0:
                        break
        if cover > 0:
            pay_rows.append(cover)

        status = ("paid" if paid >= patient_due and patient_due >= 0 and (paid + cover) > 0 else
                  "partial" if paid > 0 else
                  "submitted" if cover > 0 else "billed")
        if self.chance(0.02):
            status = "written_off"

        self.bills.append((
            bill_id, f"BILL{self.next_id('bill'):06d}", enc_id, patient["id"], insurance_id, day,
            subtotal, discount, cover, patient_due, round(paid + cover, 2), status,
            None, at(day, 17, 0)))

        for amount in pay_rows:
            method = "insurance" if amount == cover and cover > 0 else \
                rng.choices(["mpesa", "cash", "bank_transfer", "cheque", "waiver"], [46, 30, 12, 8, 4])[0]
            self.payments.append((
                self.next_id("pay"), bill_id, patient["id"],
                at(day + dt.timedelta(days=rng.randint(0, 45)), rng.randint(8, 18), rng.randint(0, 59)),
                amount, method, self.pick(self.clerks)["id"],
                (f"MPE{rng.randint(100000, 999999)}" if method == "mpesa" else
                 f"CLM{rng.randint(100000, 999999)}" if method == "insurance" else
                 f"RCT{rng.randint(100000, 999999)}"),
                "Waiver approved by the medical superintendent." if method == "waiver" else None))

        if cover > 0 and insurance_id:
            claim_status = rng.choices(["paid", "approved", "partially_approved", "under_review",
                                        "submitted", "rejected"], [34, 20, 14, 12, 12, 8])[0]
            approved = (cover if claim_status in ("paid", "approved") else
                        round(cover * self.pick([0.5, 0.65, 0.8]), 2) if claim_status == "partially_approved" else
                        0.0 if claim_status == "rejected" else None)
            decision = day + dt.timedelta(days=rng.randint(5, 60)) if claim_status not in ("submitted", "under_review") else None
            self.claims.append((
                self.next_id("claim"), f"CLM{self.ids['claim']:07d}", bill_id, insurance_id,
                day + dt.timedelta(days=rng.randint(1, 10)), cover, approved, claim_status, decision,
                self.pick(["Pre-authorisation missing", "Service not covered under the policy",
                           "Documentation incomplete", "Benefit limit exhausted"])
                if claim_status in ("rejected", "partially_approved") else None))

    # -- emit everything ------------------------------------------------------

    def write_clinical(self) -> None:
        w = self.w
        w.comment(f"Appointments ({len(self.appointments)}) — bookings against a named provider and clinic slot")
        w.insert("appointments",
                 ["id", "appointment_no", "patient_id", "provider_id", "department_id", "schedule_id",
                  "scheduled_start", "scheduled_end", "appointment_type", "status", "booking_channel",
                  "booked_at", "booked_by", "reason", "checked_in_at", "seen_at", "wait_minutes",
                  "encounter_id", "cancelled_at", "cancellation_reason", "rescheduled_from", "notes"],
                 self.appointments)

        w.comment(f"Encounters ({len(self.encounters)})")
        w.insert("encounters",
                 ["id", "encounter_no", "patient_id", "appointment_id", "encounter_date", "encounter_type",
                  "department", "department_id", "chief_complaint", "provider_id", "attending_provider_id",
                  "referred_by_id", "ward", "bed_no", "arrival_mode", "status", "disposition",
                  "discharge_date", "discharge_notes", "follow_up_date", "created_at"],
                 self.encounters)

        w.comment(f"Emergency triage ({len(self.triage)})")
        w.insert("triage_assessments",
                 ["id", "encounter_id", "patient_id", "triaged_by", "arrival_time", "triage_time",
                  "seen_time", "triage_category", "triage_colour", "presenting_complaint",
                  "door_to_triage_minutes", "door_to_doctor_minutes", "notes"], self.triage)

        w.comment(f"Vital signs ({len(self.vitals)})")
        w.insert("vital_signs",
                 ["id", "encounter_id", "patient_id", "recorded_by", "temperature_c", "pulse_bpm",
                  "resp_rate", "bp_systolic", "bp_diastolic", "spo2_pct", "weight_kg", "height_cm",
                  "pain_score", "gcs_score", "blood_glucose", "news2_score", "recorded_at"], self.vitals)

        w.comment(f"Diagnoses ({len(self.diagnoses)})")
        w.insert("diagnoses",
                 ["id", "encounter_id", "patient_id", "icd10_code", "diagnosis_desc", "dx_type",
                  "diagnosed_by", "diagnosed_at", "certainty", "is_active", "resolved_at", "notes"],
                 self.diagnoses)

        w.comment(f"Clinical notes ({len(self.notes)})")
        w.insert("clinical_notes",
                 ["id", "encounter_id", "patient_id", "author_id", "note_type", "note_datetime",
                  "subjective", "objective", "assessment", "plan", "body", "is_signed", "signed_at"],
                 self.notes)

        w.comment(f"Admissions ({len(self.admissions)})")
        w.insert("admissions",
                 ["id", "encounter_id", "patient_id", "admitting_dr", "attending_dr", "admission_date",
                  "ward", "ward_id", "bed_no", "admission_type", "admission_source", "admitting_dx",
                  "is_readmission_30d", "discharge_date", "discharge_type", "discharge_summary"],
                 self.admissions)

        w.comment(f"Bed assignments ({len(self.bed_assignments)})")
        w.insert("bed_assignments",
                 ["id", "admission_id", "bed_id", "ward_id", "assigned_at", "released_at", "assigned_by"],
                 self.bed_assignments)

        w.comment(f"Ward transfers ({len(self.transfers)})")
        w.insert("patient_transfers",
                 ["id", "admission_id", "from_ward_id", "to_ward_id", "transferred_at", "reason", "authorised_by"],
                 self.transfers)

        w.comment(f"Referrals ({len(self.referrals)})")
        w.insert("referrals",
                 ["id", "referral_no", "patient_id", "encounter_id", "referring_provider_id",
                  "from_department_id", "to_department_id", "to_provider_id", "external_facility",
                  "direction", "urgency", "reason", "clinical_summary", "status", "referred_on",
                  "responded_on", "outcome"], self.referrals)

        w.comment(f"Procedures ({len(self.procedures)})")
        w.insert("procedures",
                 ["id", "encounter_id", "patient_id", "procedure_code", "procedure_name", "performed_by",
                  "performed_at", "location", "anaesthesia", "outcome", "complication_note", "notes"],
                 self.procedures)

        w.comment(f"Theatre operations ({len(self.surgeries)})")
        w.insert("surgeries",
                 ["id", "procedure_id", "encounter_id", "patient_id", "theatre_no", "primary_surgeon",
                  "assistant_surgeon", "anaesthetist_id", "scrub_nurse_id", "urgency", "asa_grade",
                  "scheduled_start", "actual_start", "actual_end", "blood_loss_ml", "status",
                  "cancellation_reason", "operative_findings"], self.surgeries)

        w.comment(f"Prescriptions ({len(self.prescriptions)})")
        w.insert("prescriptions",
                 ["id", "rx_number", "encounter_id", "patient_id", "prescriber_id", "issue_date",
                  "valid_until", "status", "dispensed_by", "dispensed_at", "notes", "created_at"],
                 self.prescriptions)

        w.comment(f"Prescription items ({len(self.rx_items)})")
        w.insert("prescription_items",
                 ["id", "prescription_id", "medication_id", "dosage", "frequency", "duration_days",
                  "quantity", "route", "instructions", "is_dispensed", "dispensed_at",
                  "unit_price_kes", "line_total_kes"], self.rx_items)

        w.comment(f"Medication administration record ({len(self.mar)})")
        w.insert("medication_administration",
                 ["id", "prescription_item_id", "admission_id", "patient_id", "administered_by",
                  "scheduled_at", "administered_at", "dose_given", "route", "status",
                  "reason_not_given", "notes"], self.mar)

        w.comment(f"Laboratory orders ({len(self.lab_orders)})")
        w.insert("lab_orders",
                 ["id", "order_no", "encounter_id", "patient_id", "ordered_by", "collected_by",
                  "order_date", "collected_at", "panel_name", "priority", "status", "specimen_type",
                  "price_kes"], self.lab_orders)

        w.comment(f"Laboratory results ({len(self.lab_results)})")
        w.insert("lab_results",
                 ["id", "order_id", "patient_id", "test_name", "result_value", "result_numeric",
                  "result_unit", "reference_range", "is_abnormal", "abnormal_flag", "is_critical",
                  "verified_by", "resulted_at", "notes"], self.lab_results)

        w.comment(f"Imaging orders and reports ({len(self.imaging)})")
        w.insert("imaging_orders",
                 ["id", "order_no", "encounter_id", "patient_id", "ordered_by", "modality", "body_part",
                  "indication", "priority", "order_date", "performed_at", "status", "findings",
                  "impression", "report", "is_abnormal", "radiologist", "reported_at", "price_kes"],
                 self.imaging)

        w.comment(f"Patient safety incidents ({len(self.incidents)})")
        w.insert("incident_reports",
                 ["id", "incident_no", "patient_id", "encounter_id", "ward_id", "department_id",
                  "reported_by", "occurred_at", "reported_at", "category", "severity", "description",
                  "immediate_action", "investigation_status", "root_cause", "actions_taken",
                  "closed_at"], self.incidents)

        w.comment(f"Clinical decision support alerts ({len(self.alerts)})")
        w.insert("cds_alerts",
                 ["id", "patient_id", "encounter_id", "alert_type", "severity", "title", "message",
                  "drug1_id", "drug2_id", "allergen_id", "triggered_at", "triggered_for",
                  "is_acknowledged", "acknowledged_by", "acknowledged_at", "override_reason"],
                 self.alerts)

        w.comment(f"Bills ({len(self.bills)})")
        w.insert("billing_encounters",
                 ["id", "bill_no", "encounter_id", "patient_id", "insurance_id", "billing_date",
                  "subtotal_kes", "discount_kes", "insurance_cover_kes", "patient_due_kes",
                  "amount_paid_kes", "status", "notes", "created_at"], self.bills)

        w.comment(f"Bill line items ({len(self.bill_items)})")
        w.insert("billing_items",
                 ["id", "billing_id", "item_type", "item_code", "description", "quantity",
                  "unit_price_kes", "total_kes"], self.bill_items)

        w.comment(f"Insurance claims ({len(self.claims)})")
        w.insert("insurance_claims",
                 ["id", "claim_no", "billing_id", "insurance_id", "submitted_on", "claimed_kes",
                  "approved_kes", "status", "decision_on", "rejection_reason"], self.claims)

        w.comment(f"Payments ({len(self.payments)})")
        w.insert("payments",
                 ["id", "billing_id", "patient_id", "payment_date", "amount_kes", "payment_method",
                  "received_by", "reference_no", "notes"], self.payments)

        w.comment(f"Antenatal visits ({len(self.anc_visits)})")
        w.insert("antenatal_visits",
                 ["id", "patient_id", "encounter_id", "visit_number", "visit_date", "gestation_weeks",
                  "fundal_height_cm", "fetal_heart_rate", "presentation", "bp_systolic", "bp_diastolic",
                  "weight_kg", "urine_protein", "haemoglobin", "risk_factors", "seen_by",
                  "next_visit_date", "notes"], self.anc_visits)

        w.comment(f"Deliveries ({len(self.deliveries)})")
        w.insert("deliveries",
                 ["id", "patient_id", "encounter_id", "admission_id", "delivered_at", "delivery_mode",
                  "labour_onset", "gestation_weeks", "labour_hours", "delivered_by", "anaesthesia",
                  "episiotomy", "perineal_tear", "blood_loss_ml", "placenta_complete",
                  "complications", "outcome"], self.deliveries)

        w.comment(f"Newborns ({len(self.newborns)})")
        w.insert("newborns",
                 ["id", "delivery_id", "mother_patient_id", "patient_id", "birth_order", "sex",
                  "birth_weight_g", "length_cm", "head_circumference_cm", "apgar_1min", "apgar_5min",
                  "resuscitation_required", "admitted_to_nbu", "outcome", "birth_notification_no",
                  "notes"], self.newborns)

        w.comment(f"Mortality records ({len(self.mortality)})")
        w.insert("mortality_records",
                 ["id", "patient_id", "encounter_id", "admission_id", "died_at", "place_of_death",
                  "immediate_cause_code", "underlying_cause_code", "contributing_causes",
                  "certified_by", "certificate_no", "autopsy_requested", "autopsy_performed",
                  "mortuary_admitted_at", "body_released_at", "notified_to_registrar", "notes"],
                 self.mortality)

        w.comment(f"Pharmacy stock batches ({len(self.stock_batches)})")
        w.insert("stock_batches",
                 ["id", "medication_id", "batch_no", "expiry_date", "quantity_received",
                  "quantity_on_hand", "unit_cost_kes", "supplier", "received_on", "received_by",
                  "store"], self.stock_batches)

        w.comment(f"Stock movements ({len(self.stock_movements)})")
        w.insert("stock_movements",
                 ["id", "medication_id", "batch_id", "movement_type", "quantity", "moved_at",
                  "moved_by", "prescription_id", "reason", "balance_after"], self.stock_movements)

        w.comment(f"Blood bank units ({len(self.blood_units)})")
        w.insert("blood_units",
                 ["id", "unit_no", "blood_group", "component", "volume_ml", "source", "collected_on",
                  "expires_on", "screening_status", "status", "stored_in"], self.blood_units)

        w.comment(f"Transfusions ({len(self.transfusions)})")
        w.insert("transfusions",
                 ["id", "patient_id", "encounter_id", "admission_id", "blood_unit_id", "requested_by",
                  "cross_match_result", "indication", "issued_at", "started_at", "completed_at",
                  "volume_transfused_ml", "administered_by", "reaction", "reaction_notes"],
                 self.transfusions)

        w.comment(f"Duty rota ({len(self.shifts)})")
        w.insert("provider_shifts",
                 ["id", "provider_id", "ward_id", "department_id", "shift_date", "shift_type",
                  "starts_at", "ends_at", "role_on_shift", "is_in_charge"], self.shifts)

        w.comment(f"Patient documents ({len(self.documents)})")
        w.insert("patient_documents",
                 ["id", "patient_id", "encounter_id", "doc_type", "title", "file_name", "mime_type",
                  "size_bytes", "uploaded_by", "uploaded_at", "is_signed", "notes"], self.documents)

        w.comment(f"Consents ({len(self.consents)})")
        w.insert("consents",
                 ["id", "patient_id", "encounter_id", "procedure_id", "consent_type", "granted",
                  "granted_by", "witness_id", "signed_at", "expires_on", "withdrawn_at", "notes"],
                 self.consents)

        w.comment("Care programmes")
        w.insert("care_programs",
                 ["id", "code", "name", "description", "target_condition", "review_interval_days",
                  "is_active"], getattr(self, "enrollments_programs", []))

        w.comment(f"Programme enrolments ({len(self.enrollments)})")
        w.insert("program_enrollments",
                 ["id", "patient_id", "program_id", "enrollment_no", "enrolled_on", "enrolled_by",
                  "status", "last_visit_date", "next_review_date", "exit_date", "exit_reason",
                  "notes"], self.enrollments)

        w.comment(f"Biomedical equipment ({len(self.equipment)})")
        w.insert("equipment",
                 ["id", "asset_no", "name", "category", "manufacturer", "model", "serial_no",
                  "department_id", "ward_id", "purchase_date", "cost_kes", "warranty_expiry",
                  "status", "last_service_date", "next_service_date"], self.equipment)

        w.comment(f"Equipment maintenance ({len(self.equipment_maint)})")
        w.insert("equipment_maintenance",
                 ["id", "equipment_id", "maintenance_type", "performed_on", "vendor", "technician",
                  "downtime_hours", "cost_kes", "outcome", "notes"], self.equipment_maint)

        w.comment(f"Notifiable disease reports ({len(self.notifiable)})")
        w.insert("notifiable_disease_reports",
                 ["id", "patient_id", "encounter_id", "diagnosis_id", "icd10_code", "disease_name",
                  "case_classification", "detected_on", "reported_on", "reported_by", "reported_to",
                  "lab_confirmed", "contact_tracing_done", "outcome", "notes"], self.notifiable)

        w.comment(f"Patient feedback ({len(self.feedback)})")
        w.insert("patient_feedback",
                 ["id", "patient_id", "encounter_id", "department_id", "submitted_on", "channel",
                  "overall_rating", "waiting_time_rating", "staff_courtesy_rating",
                  "cleanliness_rating", "would_recommend", "comments", "follow_up_required",
                  "resolved_on"], self.feedback)

        w.comment(f"Record access log ({len(self.access_log)})")
        w.insert("record_access_log",
                 ["patient_id", "provider_id", "encounter_id", "accessed_at", "access_type",
                  "module", "reason", "ip_address", "is_break_glass"], self.access_log)

        w.comment("Mark beds currently occupied by an open admission")
        w.parts.append(
            "UPDATE beds SET status = 'occupied' WHERE id IN "
            "(SELECT bed_id FROM bed_assignments WHERE released_at IS NULL);\n"
            "UPDATE beds SET status = 'maintenance' WHERE id IN "
            "(SELECT id FROM beds WHERE status = 'available' ORDER BY id LIMIT 4);\n")

        w.comment("Reset sequences so future manual inserts do not collide")
        for table in ["departments", "wards", "beds", "providers", "provider_licenses",
                      "provider_schedules", "provider_time_off", "medication_catalog",
                      "drug_interactions", "allergen_catalog", "lab_test_catalog", "vaccine_catalog",
                      "insurance_providers", "patient_allergies", "patient_medical_history",
                      "patient_family_history", "immunizations", "patient_insurance",
                      "triage_assessments", "vital_signs", "diagnoses", "clinical_notes",
                      "admissions", "bed_assignments", "patient_transfers", "referrals",
                      "procedures", "surgeries", "prescription_items", "medication_administration",
                      "lab_results", "cds_alerts", "billing_items", "insurance_claims", "payments",
                      "antenatal_visits", "deliveries", "newborns", "mortality_records",
                      "stock_batches", "stock_movements", "blood_units", "transfusions",
                      "provider_shifts", "patient_documents", "consents", "care_programs",
                      "program_enrollments", "equipment", "equipment_maintenance",
                      "notifiable_disease_reports", "patient_feedback", "incident_reports",
                      "record_access_log"]:
            w.parts.append(
                f"SELECT setval(pg_get_serial_sequence('{table}', 'id'), "
                f"COALESCE((SELECT MAX(id) FROM {table}), 1));\n")


TRUNCATE_ORDER = """
TRUNCATE TABLE
  record_access_log, patient_feedback, notifiable_disease_reports, incident_reports,
  equipment_maintenance, equipment, program_enrollments, care_programs,
  consents, patient_documents, provider_shifts, transfusions, blood_units,
  stock_movements, stock_batches, mortality_records, newborns, deliveries,
  antenatal_visits,
  payments, insurance_claims, billing_items, billing_encounters, patient_insurance,
  cds_alerts, imaging_orders, lab_results, lab_orders, medication_administration,
  prescription_items, prescriptions, surgeries, procedures, referrals,
  patient_transfers, bed_assignments, admissions, clinical_notes, diagnoses,
  vital_signs, triage_assessments, encounters, appointments,
  immunizations, patient_family_history, patient_medical_history, patient_allergies,
  patients, provider_time_off, provider_schedules, provider_licenses,
  drug_interactions, medication_catalog, allergen_catalog, lab_test_catalog,
  vaccine_catalog, procedure_codes, beds, wards, providers, departments,
  insurance_providers, icd10_codes, counties
  RESTART IDENTITY CASCADE;
"""


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--patients", type=int, default=200)
    parser.add_argument("--seed", type=int, default=20260904)
    parser.add_argument("--out", type=Path,
                        default=Path(__file__).parent / "init" / "02_seed.sql")
    args = parser.parse_args()

    rng = random.Random(args.seed)
    hospital = Hospital(rng, args.patients)
    hospital.gen_reference()
    hospital.gen_providers()
    hospital.gen_patients()
    hospital.gen_patient_background()
    hospital.gen_journeys()
    hospital.gen_ancillary()
    hospital.write_clinical()

    counts = hospital.w.counts
    total = sum(counts.values())
    summary = "\n".join(f"--   {name:<28} {n:>6}" for name, n in sorted(counts.items()))
    header = f"""-- ============================================================================
-- Synthetic Hospital EMR — auto-generated seed data.
--
-- DO NOT EDIT BY HAND. Regenerate with:
--   python docker/dev-postgres/generate_seed.py --patients {args.patients} --seed {args.seed}
--
-- Deterministic: the same --seed reproduces this file byte for byte.
-- Loads after 01_schema.sql on a fresh volume, and can be replayed by hand
-- (the TRUNCATE below makes it idempotent).
--
-- Clinical "today" is {TODAY}; history starts {HISTORY_START} and bookings
-- run to {FUTURE_END}. All amounts are in KES.
--
-- Rows ({total} total):
{summary}
-- ============================================================================

SET client_encoding = 'UTF8';
SET session_replication_role = 'replica';  -- defer FK checks during bulk load
{TRUNCATE_ORDER}"""

    footer = "\nSET session_replication_role = 'origin';\nANALYZE;\n"
    args.out.write_text(header + hospital.w.render() + footer, encoding="utf-8")
    print(f"wrote {args.out} ({total} rows across {len(counts)} tables)")
    for name, n in sorted(counts.items(), key=lambda kv: -kv[1]):
        print(f"  {name:<30} {n:>6}")


if __name__ == "__main__":
    main()
